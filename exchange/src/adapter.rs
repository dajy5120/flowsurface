mod client;
mod connect;
mod hub;
mod limiter;
pub mod proxy;

pub use super::error::AdapterError;
pub use connect::STALE_REASON;

/// K 线之外的衍生品序列（docs/63 §10 J3：图上指标的资金费、溢价、标记价、多空比、主动买卖比）。持仓走 `fetch_open_interest`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DerivKind {
    Funding,
    Premium,
    Mark,
    LsRatio,
    TakerRatio,
}

/// 一个数据点：`avail` = 这个值**能被知道**的时刻（毫秒；资金费 = 结算时刻、溢价 / 标记价 K 线 = 收盘、
/// 统计类 = 统计时段结束），宿主按它对齐、不前视。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DerivPoint {
    pub avail: u64,
    pub value: f64,
}
use super::{Ticker, Timeframe};
use crate::{
    Kline, Price, PushFrequency, TickMultiplier, TickerInfo, Trade, UnixMs, depth::Depth, unit::Qty,
};

use enum_map::{Enum, EnumMap};
use futures::SinkExt;
use rustc_hash::{FxHashMap, FxHashSet};
use serde::{Deserialize, Serialize};
use std::{str::FromStr, sync::Arc, time::Duration};

pub use client::{AdapterHandles, MAX_KLINE_STREAMS_PER_STREAM, MAX_TRADE_TICKERS_PER_STREAM};
pub use proxy::Proxy;

/// 逐笔攒批的间隔。各交易所的流都是「收到一条消息时，距上次下发够这么久才下发」——**没有定时器**，
/// 所以间隔 > 0 时，一串里第一笔之后的会被扣到下一串来了才下发：币安 U 本位永续的逐笔一串十几笔挤在 ~2 ms 内、
/// 串与串隔 ~150 ms（2026-10-27 实测），原来的 33 ms 让图上的最新价总晚一串、显示的是这串的第一笔。
/// 现在每条消息到了就下发；界面那边一帧之内来的事件在同一帧里处理、只画一次（iced 按帧合并重画）。
const TRADE_BUCKET_INTERVAL: Duration = Duration::ZERO;

/// 下发事件里的更新时刻对齐到的格子（毫秒）：下游（热力图等）按它分桶，保持上游原来的 33 ms。
/// 与上面的下发节奏无关——那个改成 0 时，这里拿它做除数就是除以零（2026-10-27 逐笔流任务因此崩溃）。
const TRADE_UPDATE_GRID_MS: u64 = 33;

pub fn allowed_multipliers_for_min_tick(min_ticksize: crate::unit::MinTicksize) -> &'static [u16] {
    hub::hyperliquid::allowed_multipliers_for_min_tick(min_ticksize)
}

async fn flush_trade_buffers<V>(
    output: &mut futures::channel::mpsc::Sender<Event>,
    ticker_info_map: &FxHashMap<Ticker, (TickerInfo, V)>,
    trade_buffers_map: &mut FxHashMap<Ticker, Vec<Trade>>,
) {
    let interval_ms = TRADE_UPDATE_GRID_MS;

    for (ticker, trades_buffer) in trade_buffers_map.iter_mut() {
        if trades_buffer.is_empty() {
            continue;
        }

        let bucket_update_t = trades_buffer
            .iter()
            .map(|t| t.time.as_u64())
            .max()
            .map(|t| UnixMs::new((t / interval_ms) * interval_ms));

        if let Some((ticker_info, _)) = ticker_info_map.get(ticker)
            && let Some(update_t) = bucket_update_t
        {
            let _ = output
                .send(Event::TradesReceived(
                    StreamKind::Trades {
                        ticker_info: *ticker_info,
                    },
                    update_t,
                    std::mem::take(trades_buffer).into_boxed_slice(),
                ))
                .await;
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize, Serialize)]
pub enum MarketKind {
    Spot,
    LinearPerps,
    InversePerps,
}

impl MarketKind {
    pub const ALL: [MarketKind; 3] = [
        MarketKind::Spot,
        MarketKind::LinearPerps,
        MarketKind::InversePerps,
    ];

    pub fn qty_in_quote_value(&self, qty: Qty, price: Price, size_in_quote_ccy: bool) -> f32 {
        let qty = qty.to_f32_lossy();

        match self {
            MarketKind::InversePerps => qty,
            _ => {
                if size_in_quote_ccy {
                    qty
                } else {
                    price.to_f32() * qty
                }
            }
        }
    }
}

impl std::fmt::Display for MarketKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}",
            match self {
                MarketKind::Spot => "Spot",
                MarketKind::LinearPerps => "Linear",
                MarketKind::InversePerps => "Inverse",
            }
        )
    }
}

impl FromStr for MarketKind {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.eq_ignore_ascii_case("spot") {
            Ok(Self::Spot)
        } else if s.eq_ignore_ascii_case("linear") {
            Ok(Self::LinearPerps)
        } else if s.eq_ignore_ascii_case("inverse") {
            Ok(Self::InversePerps)
        } else {
            Err(format!("Invalid market kind: {}", s))
        }
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash, Deserialize, Serialize)]
pub enum StreamKind {
    Kline {
        ticker_info: TickerInfo,
        timeframe: Timeframe,
    },
    Depth {
        ticker_info: TickerInfo,
        #[serde(default = "default_depth_aggr")]
        depth_aggr: StreamTicksize,
        push_freq: PushFrequency,
    },
    Trades {
        ticker_info: TickerInfo,
    },
}

impl StreamKind {
    pub fn ticker_info(&self) -> TickerInfo {
        match self {
            StreamKind::Kline { ticker_info, .. }
            | StreamKind::Depth { ticker_info, .. }
            | StreamKind::Trades { ticker_info, .. } => *ticker_info,
        }
    }

    pub fn as_depth_stream(&self) -> Option<(TickerInfo, StreamTicksize, PushFrequency)> {
        match self {
            StreamKind::Depth {
                ticker_info,
                depth_aggr,
                push_freq,
            } => Some((*ticker_info, *depth_aggr, *push_freq)),
            _ => None,
        }
    }

    pub fn as_trade_stream(&self) -> Option<TickerInfo> {
        match self {
            StreamKind::Trades { ticker_info } => Some(*ticker_info),
            _ => None,
        }
    }

    pub fn as_kline_stream(&self) -> Option<(TickerInfo, Timeframe)> {
        match self {
            StreamKind::Kline {
                ticker_info,
                timeframe,
            } => Some((*ticker_info, *timeframe)),
            _ => None,
        }
    }
}

#[derive(Debug, Default)]
pub struct UniqueStreams {
    streams: EnumMap<Exchange, Option<FxHashMap<TickerInfo, FxHashSet<StreamKind>>>>,
    specs: EnumMap<Exchange, Option<StreamSpecs>>,
}

impl UniqueStreams {
    pub fn from<'a>(streams: impl Iterator<Item = &'a StreamKind>) -> Self {
        let mut unique_streams = UniqueStreams::default();
        for stream in streams {
            unique_streams.add(*stream);
        }
        unique_streams
    }

    pub fn add(&mut self, stream: StreamKind) {
        let (exchange, ticker_info) = match stream {
            StreamKind::Kline { ticker_info, .. }
            | StreamKind::Depth { ticker_info, .. }
            | StreamKind::Trades { ticker_info, .. } => (ticker_info.exchange(), ticker_info),
        };

        self.streams[exchange]
            .get_or_insert_with(FxHashMap::default)
            .entry(ticker_info)
            .or_default()
            .insert(stream);

        self.update_specs_for_exchange(exchange);
    }

    pub fn extend<'a>(&mut self, streams: impl IntoIterator<Item = &'a StreamKind>) {
        for stream in streams {
            self.add(*stream);
        }
    }

    fn update_specs_for_exchange(&mut self, exchange: Exchange) {
        let depth_streams = self.depth_streams(Some(exchange));
        let trade_streams = self.trade_streams(Some(exchange));
        let kline_streams = self.kline_streams(Some(exchange));

        self.specs[exchange] = Some(StreamSpecs {
            depth: depth_streams,
            trade: trade_streams,
            kline: kline_streams,
        });
    }

    fn streams<T, F>(&self, exchange_filter: Option<Exchange>, stream_extractor: F) -> Vec<T>
    where
        F: Fn(Exchange, &StreamKind) -> Option<T>,
    {
        let f = &stream_extractor;

        let per_exchange = |exchange| {
            self.streams[exchange]
                .as_ref()
                .into_iter()
                .flat_map(|ticker_map| ticker_map.values().flatten())
                .filter_map(move |stream| f(exchange, stream))
        };

        match exchange_filter {
            Some(exchange) => per_exchange(exchange).collect(),
            None => Exchange::ALL.into_iter().flat_map(per_exchange).collect(),
        }
    }

    pub fn depth_streams(
        &self,
        exchange_filter: Option<Exchange>,
    ) -> Vec<(TickerInfo, StreamTicksize, PushFrequency)> {
        self.streams(exchange_filter, |_, stream| stream.as_depth_stream())
    }

    pub fn kline_streams(&self, exchange_filter: Option<Exchange>) -> Vec<(TickerInfo, Timeframe)> {
        self.streams(exchange_filter, |_, stream| stream.as_kline_stream())
    }

    pub fn trade_streams(&self, exchange_filter: Option<Exchange>) -> Vec<TickerInfo> {
        self.streams(exchange_filter, |_, stream| stream.as_trade_stream())
    }

    pub fn combined_used(&self) -> impl Iterator<Item = (Exchange, &StreamSpecs)> {
        self.specs
            .iter()
            .filter_map(|(exchange, specs)| specs.as_ref().map(|stream| (exchange, stream)))
    }

    pub fn combined(&self) -> &EnumMap<Exchange, Option<StreamSpecs>> {
        &self.specs
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash, Deserialize, Serialize)]
pub enum StreamTicksize {
    ServerSide(TickMultiplier),
    #[default]
    Client,
}

fn default_depth_aggr() -> StreamTicksize {
    StreamTicksize::Client
}

#[derive(Debug, Clone, Default)]
pub struct StreamSpecs {
    pub depth: Vec<(TickerInfo, StreamTicksize, PushFrequency)>,
    pub trade: Vec<TickerInfo>,
    pub kline: Vec<(TickerInfo, Timeframe)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize, Serialize)]
pub enum Venue {
    Bybit,
    Binance,
    Hyperliquid,
    Okex,
    Mexc,
}

impl Venue {
    pub const ALL: [Venue; 5] = [
        Venue::Bybit,
        Venue::Binance,
        Venue::Hyperliquid,
        Venue::Okex,
        Venue::Mexc,
    ];
}

impl std::fmt::Display for Venue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}",
            match self {
                Venue::Bybit => "Bybit",
                Venue::Binance => "Binance",
                Venue::Hyperliquid => "Hyperliquid",
                Venue::Okex => "OKX",
                Venue::Mexc => "MEXC",
            }
        )
    }
}

impl FromStr for Venue {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.eq_ignore_ascii_case("bybit") {
            Ok(Self::Bybit)
        } else if s.eq_ignore_ascii_case("binance") {
            Ok(Self::Binance)
        } else if s.eq_ignore_ascii_case("hyperliquid") {
            Ok(Self::Hyperliquid)
        } else if s.eq_ignore_ascii_case("okx") || s.eq_ignore_ascii_case("okex") {
            Ok(Self::Okex)
        } else if s.eq_ignore_ascii_case("mexc") {
            Ok(Self::Mexc)
        } else {
            Err(format!("Invalid venue: {}", s))
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize, Serialize, Enum)]
pub enum Exchange {
    BinanceLinear,
    BinanceInverse,
    BinanceSpot,
    BybitLinear,
    BybitInverse,
    BybitSpot,
    HyperliquidLinear,
    HyperliquidSpot,
    OkexLinear,
    OkexInverse,
    OkexSpot,
    MexcLinear,
    MexcInverse,
    MexcSpot,
}

impl std::fmt::Display for Exchange {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} {}", self.venue(), self.market_type())
    }
}

impl FromStr for Exchange {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let mut parts = s.split_whitespace();
        let Some(venue_part) = parts.next() else {
            return Err(format!("Invalid exchange: {}", s));
        };
        let Some(market_part) = parts.next() else {
            return Err(format!("Invalid exchange: {}", s));
        };

        if parts.next().is_some() {
            return Err(format!("Invalid exchange: {}", s));
        }

        let venue = Venue::from_str(venue_part).map_err(|_| format!("Invalid exchange: {}", s))?;
        let market =
            MarketKind::from_str(market_part).map_err(|_| format!("Invalid exchange: {}", s))?;

        Self::from_venue_and_market(venue, market).ok_or_else(|| format!("Invalid exchange: {}", s))
    }
}

impl Exchange {
    pub const ALL: [Exchange; 14] = [
        Exchange::BinanceLinear,
        Exchange::BinanceInverse,
        Exchange::BinanceSpot,
        Exchange::BybitLinear,
        Exchange::BybitInverse,
        Exchange::BybitSpot,
        Exchange::HyperliquidLinear,
        Exchange::HyperliquidSpot,
        Exchange::OkexLinear,
        Exchange::OkexInverse,
        Exchange::OkexSpot,
        Exchange::MexcLinear,
        Exchange::MexcInverse,
        Exchange::MexcSpot,
    ];

    pub fn from_venue_and_market(venue: Venue, market: MarketKind) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|exchange| exchange.venue() == venue && exchange.market_type() == market)
    }

    pub fn market_type(&self) -> MarketKind {
        match self {
            Exchange::BinanceLinear
            | Exchange::BybitLinear
            | Exchange::HyperliquidLinear
            | Exchange::OkexLinear
            | Exchange::MexcLinear => MarketKind::LinearPerps,
            Exchange::BinanceInverse
            | Exchange::BybitInverse
            | Exchange::OkexInverse
            | Exchange::MexcInverse => MarketKind::InversePerps,
            Exchange::BinanceSpot
            | Exchange::BybitSpot
            | Exchange::HyperliquidSpot
            | Exchange::OkexSpot
            | Exchange::MexcSpot => MarketKind::Spot,
        }
    }

    pub fn venue(&self) -> Venue {
        match self {
            Exchange::BybitLinear | Exchange::BybitInverse | Exchange::BybitSpot => Venue::Bybit,
            Exchange::BinanceLinear | Exchange::BinanceInverse | Exchange::BinanceSpot => {
                Venue::Binance
            }
            Exchange::HyperliquidLinear | Exchange::HyperliquidSpot => Venue::Hyperliquid,
            Exchange::OkexLinear | Exchange::OkexInverse | Exchange::OkexSpot => Venue::Okex,
            Exchange::MexcLinear | Exchange::MexcInverse | Exchange::MexcSpot => Venue::Mexc,
        }
    }

    pub fn is_depth_client_aggr(&self) -> bool {
        !matches!(
            self,
            Exchange::HyperliquidLinear | Exchange::HyperliquidSpot
        )
    }

    pub fn is_custom_push_freq(&self) -> bool {
        matches!(
            self,
            Exchange::BybitLinear | Exchange::BybitInverse | Exchange::BybitSpot
        )
    }

    pub fn supports_heatmap_timeframe(&self, tf: Timeframe) -> bool {
        match self {
            Exchange::BybitSpot
            | Exchange::MexcSpot
            | Exchange::MexcInverse
            | Exchange::MexcLinear => {
                tf != Timeframe::MS100 && tf != Timeframe::MS300 && tf != Timeframe::MS500
            }
            Exchange::BybitLinear | Exchange::BybitInverse => tf != Timeframe::MS200,
            Exchange::HyperliquidLinear | Exchange::HyperliquidSpot => {
                tf != Timeframe::MS100 && tf != Timeframe::MS200 && tf != Timeframe::MS300
            }
            _ => true,
        }
    }

    pub fn supports_kline_timeframe(&self, tf: Timeframe) -> bool {
        match self.venue() {
            Venue::Binance | Venue::Bybit | Venue::Hyperliquid | Venue::Okex => {
                Timeframe::KLINE.contains(&tf)
            }
            Venue::Mexc => {
                Timeframe::KLINE.contains(&tf)
                    && !matches!(tf, Timeframe::M3 | Timeframe::H2 | Timeframe::H12)
            }
        }
    }

    pub fn is_perps(&self) -> bool {
        matches!(
            self,
            Exchange::BinanceLinear
                | Exchange::BinanceInverse
                | Exchange::BybitLinear
                | Exchange::BybitInverse
                | Exchange::HyperliquidLinear
                | Exchange::OkexLinear
                | Exchange::OkexInverse
                | Exchange::MexcLinear
                | Exchange::MexcInverse
        )
    }

    pub fn stream_ticksize(
        &self,
        multiplier: Option<TickMultiplier>,
        server_fallback: TickMultiplier,
    ) -> StreamTicksize {
        if self.is_depth_client_aggr() {
            StreamTicksize::Client
        } else {
            StreamTicksize::ServerSide(multiplier.unwrap_or(server_fallback))
        }
    }

    pub fn is_symbol_supported(&self, symbol: &str, log: bool) -> bool {
        let valid_symbol = symbol
            .chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == '-');

        if valid_symbol {
            return true;
        } else if log {
            log::warn!("Unsupported ticker: '{}': {:?}", self, symbol,);
        }
        false
    }
}

#[derive(Debug, Clone)]
pub enum Event {
    Connected(Exchange),
    Disconnected(Exchange, String),
    DepthReceived(StreamKind, UnixMs, Arc<Depth>),
    TradesReceived(StreamKind, UnixMs, Box<[Trade]>),
    KlineReceived(StreamKind, Kline),
}

#[derive(Debug, Clone, Hash)]
pub struct StreamConfig<I> {
    pub id: I,
    pub exchange: Exchange,
    pub tick_mltp: Option<TickMultiplier>,
    pub push_freq: PushFrequency,
}

impl<I> StreamConfig<I> {
    pub fn new(
        id: I,
        exchange: Exchange,
        tick_mltp: Option<TickMultiplier>,
        push_freq: PushFrequency,
    ) -> Self {
        Self {
            id,
            exchange,
            tick_mltp,
            push_freq,
        }
    }
}

#[cfg(test)]
mod flush_tests {
    use super::*;

    #[tokio::test]
    async fn every_message_flushes_and_update_time_stays_on_the_grid() {
        // 下发节奏是 0（每条就发）；更新时刻仍按 33 ms 的格子对齐——拿下发节奏做除数会除以零
        assert_eq!(TRADE_BUCKET_INTERVAL, Duration::ZERO);
        let ti = TickerInfo::new(Ticker::new("BTCUSDT", Exchange::BinanceLinear), 0.1, 0.001, None);
        let mut infos: FxHashMap<Ticker, (TickerInfo, ())> = FxHashMap::default();
        infos.insert(ti.ticker, (ti, ()));
        let mut bufs: FxHashMap<Ticker, Vec<Trade>> = FxHashMap::default();
        bufs.insert(ti.ticker, vec![Trade { time: UnixMs::new(1_000_050), is_sell: false, price: crate::Price::from_f32(1.0), qty: crate::unit::Qty::from_f32(1.0) }]);
        let (mut tx, mut rx) = futures::channel::mpsc::channel(4);
        flush_trade_buffers(&mut tx, &infos, &mut bufs).await;
        match rx.try_next() {
            Ok(Some(Event::TradesReceived(_, t, b))) => {
                assert_eq!(t.as_u64() % TRADE_UPDATE_GRID_MS, 0);
                assert_eq!(b.len(), 1);
            }
            _ => panic!("没下发"),
        }
        assert!(bufs[&ti.ticker].is_empty());
    }
}
