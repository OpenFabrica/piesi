//! Piesi 补丁：7z 的 LZMA2 编解码交给 liblzma（xz 项目的 C 实现），替换 lzma-rust2 的纯 Rust 实现。
//!
//! 7z 的 LZMA2 packed stream 就是 liblzma raw LZMA2 过滤器产出的位流，两边可以直接互换。
//! 多线程照 7-Zip 的做法：
//! - 编码：把输入按 chunk_size 切块，每块独立跑一个 raw 编码器（块首天然带字典重置），
//!   去掉每块末尾的 0x00 结束标记后按序拼接，整条流最后补一个 0x00。
//! - 解码：按「字典重置」chunk（控制字节 ≥ 0xE0 或 == 0x01）切成独立单元，每个单元交给一个
//!   独立的 raw 解码器并行解，再按顺序交出，结果与单线程逐字节一致。
//!
//! 不用 liblzma 自带的多线程（MtStreamBuilder）：那是 .xz 容器层的多块，7z 用的是裸 LZMA2 流。
//! LZMA1 与 BCJ/Delta 过滤器仍走 lzma-rust2。

use std::{
    collections::BTreeMap,
    io::{self, BufReader, Read},
    panic::{self, AssertUnwindSafe},
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
        mpsc,
    },
    thread,
};

use ::liblzma::stream::{Action, Filters, Status, Stream};

/// 单线程编解码时输出暂存区的大小。
const OUT_BUF_SIZE: usize = 64 << 10;

/// 解码单元解压后超过这个大小就不再整单元进内存，剩余部分切回单线程流式解码。
/// 取 7-Zip 的 LZMA2 块大小上限（kMaxSize = 256 MiB），7zz 写出的包全都能走并行路径。
const MAX_UNIT_UNPACKED: u64 = 256 << 20;

/// 解码时每个线程允许在飞（已派发、还没按序交出）的解压后字节数预算；总预算 = 线程数 × 它。
const IN_FLIGHT_BUDGET_PER_THREAD: u64 = 64 << 20;

/// 解压后小于这个大小的 LZMA2 流不值得起线程（7-Zip 的多线程块至少 1 MiB），直接单线程解。
pub(crate) const MT_MIN_UNPACKED: u64 = 1 << 20;

/// 把 liblzma 的错误转成 io::Error（liblzma crate 自带 From，这里只是收口）。
fn lz_err(e: ::liblzma::stream::Error) -> io::Error {
    e.into()
}

/// 建一个只带 LZMA2 过滤器、字典为 `dict_size` 的 raw 解码器。
fn raw_decoder(dict_size: u32) -> io::Result<Stream> {
    let mut opts = ::liblzma::stream::LzmaOptions::new();
    opts.dict_size(dict_size);
    let mut filters = Filters::new();
    filters.lzma2(&opts);
    Stream::new_raw_decoder(&filters).map_err(lz_err)
}

// ───────────────────────── 有序线程池（编码、解码共用） ─────────────────────────

/// 把「一个单元进、一段字节出」的任务分给最多 `max_threads` 个线程，结果按派发顺序取回。
/// 线程按需起（小输入只会起一个）；任务通道容量为 0，派发时直接交到空闲线程手里，
/// 没有空闲线程又起满了就等，所以在飞单元数天然不超过线程数。
struct OrderedPool<J: Send + 'static> {
    job_tx: mpsc::SyncSender<(u64, J)>,
    job_rx: Arc<Mutex<mpsc::Receiver<(u64, J)>>>,
    done_tx: mpsc::Sender<(u64, io::Result<Vec<u8>>)>,
    done_rx: mpsc::Receiver<(u64, io::Result<Vec<u8>>)>,
    work: Arc<dyn Fn(J) -> io::Result<Vec<u8>> + Send + Sync>,
    /// 正在等任务的线程数；为 0 且还没起满时派发前再起一个
    idle: Arc<AtomicUsize>,
    spawned: usize,
    max_threads: usize,
    /// 先到但序号还没轮到的结果
    out_of_order: BTreeMap<u64, io::Result<Vec<u8>>>,
    next_dispatch: u64,
    next_return: u64,
}

impl<J: Send + 'static> OrderedPool<J> {
    /// 建池但先不起线程。
    fn new<F>(threads: u32, work: F) -> Self
    where
        F: Fn(J) -> io::Result<Vec<u8>> + Send + Sync + 'static,
    {
        let (job_tx, job_rx) = mpsc::sync_channel(0);
        let (done_tx, done_rx) = mpsc::channel();
        Self {
            job_tx,
            job_rx: Arc::new(Mutex::new(job_rx)),
            done_tx,
            done_rx,
            work: Arc::new(work),
            idle: Arc::new(AtomicUsize::new(0)),
            spawned: 0,
            max_threads: threads.clamp(1, 256) as usize,
            out_of_order: BTreeMap::new(),
            next_dispatch: 0,
            next_return: 0,
        }
    }

    fn max_threads(&self) -> usize {
        self.max_threads
    }

    /// 已派发、还没按序交出的单元数。
    fn in_flight(&self) -> usize {
        (self.next_dispatch - self.next_return) as usize
    }

    /// 起一个工作线程：领任务时持锁，任务通道关闭（池被 drop）就退出。
    fn spawn_worker(&mut self) {
        let job_rx = Arc::clone(&self.job_rx);
        let done_tx = self.done_tx.clone();
        let work = Arc::clone(&self.work);
        let idle = Arc::clone(&self.idle);
        self.spawned += 1;
        thread::spawn(move || {
            loop {
                idle.fetch_add(1, Ordering::AcqRel);
                let job = job_rx.lock().unwrap_or_else(|e| e.into_inner()).recv();
                idle.fetch_sub(1, Ordering::AcqRel);
                let Ok((seq, job)) = job else { break };
                // 池自己也持有一份 done_tx，worker 若 panic 后直接退出，主线程的 recv 会永远等不到这个序号；
                // 这里是跨线程调 C 的边界，把 panic 接住变成错误发回去
                let result = panic::catch_unwind(AssertUnwindSafe(|| work(job)))
                    .unwrap_or_else(|_| Err(io::Error::other("LZMA2 worker panicked")));
                if done_tx.send((seq, result)).is_err() {
                    break;
                }
            }
        });
    }

    /// 派发一个单元；没有空闲线程就先起一个，起满了就阻塞到有线程空出来。
    fn dispatch(&mut self, job: J) -> io::Result<()> {
        if self.spawned < self.max_threads && self.idle.load(Ordering::Acquire) == 0 {
            self.spawn_worker();
        }
        self.job_tx
            .send((self.next_dispatch, job))
            .map_err(|_| io::Error::other("LZMA2 worker threads have exited"))?;
        self.next_dispatch += 1;
        Ok(())
    }

    /// 收一条结果进乱序表；`block` 为假时没结果就立刻返回 false。
    fn collect_one(&mut self, block: bool) -> io::Result<bool> {
        let received = if block {
            self.done_rx.recv().ok()
        } else {
            match self.done_rx.try_recv() {
                Ok(r) => Some(r),
                Err(mpsc::TryRecvError::Empty) => return Ok(false),
                Err(mpsc::TryRecvError::Disconnected) => None,
            }
        };
        let (seq, result) =
            received.ok_or_else(|| io::Error::other("LZMA2 worker threads have exited"))?;
        self.out_of_order.insert(seq, result);
        Ok(true)
    }

    /// 按派发顺序取下一个结果。`block` 为假且结果还没好就返回 None；没有在飞单元也返回 None。
    fn take_next(&mut self, block: bool) -> io::Result<Option<Vec<u8>>> {
        if self.in_flight() == 0 {
            return Ok(None);
        }
        loop {
            if let Some(result) = self.out_of_order.remove(&self.next_return) {
                self.next_return += 1;
                return result.map(Some);
            }
            if !self.collect_one(block)? {
                return Ok(None);
            }
        }
    }
}

// ───────────────────────── 解码 ─────────────────────────

/// 单线程 LZMA2 解码：liblzma raw 解码器套在带缓冲的 Read 上。
pub(crate) struct Lzma2Decoder<R: Read>(::liblzma::bufread::XzDecoder<BufReader<R>>);

impl<R: Read> Lzma2Decoder<R> {
    pub(crate) fn new(input: R, dict_size: u32) -> io::Result<Self> {
        let stream = raw_decoder(dict_size)?;
        Ok(Self(::liblzma::bufread::XzDecoder::new_stream(
            BufReader::with_capacity(OUT_BUF_SIZE, input),
            stream,
        )))
    }
}

impl<R: Read> Read for Lzma2Decoder<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.0.read(buf)
    }
}

/// 解码线程的任务：一个独立单元的压缩字节 + 头里声明的解压后大小（用来一次分配够）。
type DecodeJob = (Vec<u8>, usize);

/// 超大单元的流式兜底：已攒下的字节接回输入前面，用单线程解码器解完剩余的整条流。
type Fallback<R> = ::liblzma::read::XzDecoder<io::Chain<io::Cursor<Vec<u8>>, BufReader<R>>>;

/// 多线程 LZMA2 解码：按字典重置块切单元并行解，输出顺序与单线程一致。
pub(crate) struct Lzma2DecoderMt<R: Read> {
    /// 切到流式兜底后被拿走
    inner: Option<BufReader<R>>,
    pool: OrderedPool<DecodeJob>,
    dict_size: u32,
    /// 正在攒的单元（原始压缩字节）与它头里声明的解压后大小
    unit: Vec<u8>,
    unit_unpacked: u64,
    /// 在飞单元解压后的总字节数，用来限内存
    in_flight_unpacked: u64,
    in_flight_budget: u64,
    /// 正在交出的结果与读到的位置
    out: Vec<u8>,
    out_pos: usize,
    /// 输入已到流尾（0x00 结束标记或 EOF）
    input_done: bool,
    /// 当前单元超限，等在飞结果都交完就切到 `fallback`
    fallback_wanted: bool,
    fallback: Option<Fallback<R>>,
}

impl<R: Read> Lzma2DecoderMt<R> {
    pub(crate) fn new(input: R, dict_size: u32, threads: u32) -> io::Result<Self> {
        // 先建一个解码器确认参数合法，省得错误要等到线程里才冒出来
        raw_decoder(dict_size)?;
        let pool = OrderedPool::new(threads, move |(unit, unpacked): DecodeJob| {
            decode_unit(dict_size, &unit, unpacked)
        });
        let in_flight_budget = pool.max_threads() as u64 * IN_FLIGHT_BUDGET_PER_THREAD;
        Ok(Self {
            inner: Some(BufReader::with_capacity(OUT_BUF_SIZE, input)),
            pool,
            dict_size,
            unit: Vec::new(),
            unit_unpacked: 0,
            in_flight_unpacked: 0,
            in_flight_budget,
            out: Vec::new(),
            out_pos: 0,
            input_done: false,
            fallback_wanted: false,
            fallback: None,
        })
    }

    /// 把攒好的单元派给线程。
    fn dispatch_unit(&mut self) -> io::Result<()> {
        if self.unit.is_empty() {
            return Ok(());
        }
        let unit = std::mem::take(&mut self.unit);
        let unpacked = std::mem::take(&mut self.unit_unpacked);
        self.in_flight_unpacked += unpacked;
        self.pool.dispatch((unit, unpacked as usize))
    }

    /// 读一个 LZMA2 chunk 追加到当前单元；遇到独立块起点（字典重置）先把攒好的单元派出去。
    /// 流尾（0x00 结束标记或 EOF）把 `input_done` 置真；单元超限把 `fallback_wanted` 置真。
    fn read_chunk(&mut self) -> io::Result<()> {
        let inner = self
            .inner
            .as_mut()
            .ok_or_else(|| io::Error::other("LZMA2 input already taken"))?;
        let mut control = [0u8; 1];
        match inner.read_exact(&mut control) {
            Ok(()) => {}
            // 没有 0x00 结束标记就到了 EOF：攒着的单元原样派出去，由解码器报「流不完整」，与单线程一致
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => {
                self.input_done = true;
                return self.dispatch_unit();
            }
            Err(e) => return Err(e),
        }
        let control = control[0];
        if control == 0x00 {
            self.unit.push(0x00);
            self.input_done = true;
            return self.dispatch_unit();
        }
        // 独立块起点：前面攒的单元补上结束标记派出去，新单元从这个 chunk 开始
        if (control >= 0xE0 || control == 0x01) && !self.unit.is_empty() {
            self.unit.push(0x00);
            self.dispatch_unit()?;
        }
        let inner = self
            .inner
            .as_mut()
            .ok_or_else(|| io::Error::other("LZMA2 input already taken"))?;
        self.unit.push(control);
        // chunk 头：压缩 chunk = 2 字节解压大小 + 2 字节压缩大小（+ 1 字节属性），大小都是「值 - 1」大端；
        // 未压缩 chunk（0x01/0x02）= 2 字节大小
        let (unpacked, packed) = if control >= 0x80 {
            let mut header = [0u8; 5];
            let len = if control >= 0xC0 { 5 } else { 4 };
            inner.read_exact(&mut header[..len])?;
            self.unit.extend_from_slice(&header[..len]);
            let unpacked = ((u64::from(control & 0x1F)) << 16)
                + u64::from(u16::from_be_bytes([header[0], header[1]]))
                + 1;
            let packed = usize::from(u16::from_be_bytes([header[2], header[3]])) + 1;
            (unpacked, packed)
        } else if control == 0x01 || control == 0x02 {
            let mut header = [0u8; 2];
            inner.read_exact(&mut header)?;
            self.unit.extend_from_slice(&header);
            let size = usize::from(u16::from_be_bytes(header)) + 1;
            (size as u64, size)
        } else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("invalid LZMA2 control byte: {control:#04X}"),
            ));
        };
        // 单元解压后会超过上限：不再往内存里攒，剩余的交给流式兜底（这个 chunk 的头已经在 unit 里，数据还在 inner 里）
        if self.unit_unpacked + unpacked > MAX_UNIT_UNPACKED {
            self.fallback_wanted = true;
            return Ok(());
        }
        self.unit_unpacked += unpacked;
        let start = self.unit.len();
        self.unit.resize(start + packed, 0);
        inner.read_exact(&mut self.unit[start..])
    }

    /// 在内存预算内尽量多派几个单元；没有在飞单元时至少派出一个（否则无法前进）。
    fn fill(&mut self) -> io::Result<()> {
        while !self.input_done && !self.fallback_wanted {
            let in_flight = self.pool.in_flight();
            if in_flight > 0
                && (in_flight >= self.pool.max_threads()
                    || self.in_flight_unpacked >= self.in_flight_budget)
            {
                break;
            }
            let before = self.pool.in_flight();
            while self.pool.in_flight() == before && !self.input_done && !self.fallback_wanted {
                self.read_chunk()?;
            }
        }
        Ok(())
    }

    /// 在飞结果都交完后切到流式兜底：攒下的字节 + 剩余输入，用一个新的 raw 解码器从头解。
    /// 单元总是从字典重置 chunk 开始，所以新解码器不需要前面的状态。
    fn start_fallback(&mut self) -> io::Result<()> {
        let inner = self
            .inner
            .take()
            .ok_or_else(|| io::Error::other("LZMA2 input already taken"))?;
        let buffered = std::mem::take(&mut self.unit);
        let stream = raw_decoder(self.dict_size)?;
        self.fallback = Some(::liblzma::read::XzDecoder::new_stream(
            io::Cursor::new(buffered).chain(inner),
            stream,
        ));
        self.fallback_wanted = false;
        Ok(())
    }
}

impl<R: Read> Read for Lzma2DecoderMt<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        loop {
            // 1) 手上有解好的数据先交出去
            if self.out_pos < self.out.len() {
                let n = buf.len().min(self.out.len() - self.out_pos);
                buf[..n].copy_from_slice(&self.out[self.out_pos..self.out_pos + n]);
                self.out_pos += n;
                return Ok(n);
            }
            // 2) 已切到流式兜底：剩下的全走它
            if let Some(fallback) = self.fallback.as_mut() {
                return fallback.read(buf);
            }
            // 3) 喂满流水线，再按序等下一个结果
            self.fill()?;
            match self.pool.take_next(true)? {
                Some(data) => {
                    self.in_flight_unpacked =
                        self.in_flight_unpacked.saturating_sub(data.len() as u64);
                    self.out = data;
                    self.out_pos = 0;
                }
                None if self.fallback_wanted => self.start_fallback()?,
                None if self.input_done => return Ok(0),
                // fill() 保证没有在飞单元时至少派出一个，这里只是防御
                None => return Err(io::Error::other("LZMA2 decoder made no progress")),
            }
        }
    }
}

/// 解一个独立单元（以字典重置 chunk 开头、以 0x00 结束标记结尾的完整 LZMA2 流）。
fn decode_unit(dict_size: u32, unit: &[u8], unpacked: usize) -> io::Result<Vec<u8>> {
    let mut stream = raw_decoder(dict_size)?;
    let mut out = Vec::with_capacity(unpacked);
    let mut input = unit;
    loop {
        let (before_in, before_out) = (stream.total_in(), stream.total_out());
        let status = stream
            .process_vec(input, &mut out, Action::Run)
            .map_err(lz_err)?;
        input = &input[(stream.total_in() - before_in) as usize..];
        if status == Status::StreamEnd {
            return Ok(out);
        }
        // 输入吃完还没见到结束标记：流被截断（与单线程解码器报的 premature eof 同类）
        if input.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "LZMA2 stream ended without end marker",
            ));
        }
        // 头里声明的大小已经写满还没结束，或者完全没有进展：流损坏，继续解只会撑爆内存或死循环
        let stalled = stream.total_out() == before_out && stream.total_in() == before_in;
        if out.len() == out.capacity() || stalled {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "corrupt LZMA2 stream",
            ));
        }
    }
}

// ───────────────────────── 编码 ─────────────────────────

/// liblzma 编码参数：从 lzma-rust2 的 LzmaOptions 翻译而来，可跨线程复制，两条实现对同一份选项行为一致。
#[cfg(feature = "compress")]
#[derive(Clone, Copy)]
struct Lzma2Params {
    dict_size: u32,
    lc: u32,
    lp: u32,
    pb: u32,
    fast: bool,
    nice_len: u32,
    hash_chain: bool,
    depth: u32,
}

#[cfg(feature = "compress")]
impl Lzma2Params {
    fn from_options(o: &lzma_rust2::LzmaOptions) -> Self {
        Self {
            dict_size: o.dict_size,
            lc: o.lc,
            lp: o.lp,
            pb: o.pb,
            fast: o.mode == lzma_rust2::EncodeMode::Fast,
            nice_len: o.nice_len,
            hash_chain: o.mf == lzma_rust2::MfType::Hc4,
            // lzma-rust2 的 0 表示自动，liblzma 同样用 0 表示自动
            depth: u32::try_from(o.depth_limit).unwrap_or(0),
        }
    }

    /// 建 raw LZMA2 编码器。预设 6 只是拿一份合法的默认值，所有会影响输出的字段随后都覆盖掉。
    fn raw_encoder(&self) -> io::Result<Stream> {
        use ::liblzma::stream::{LzmaOptions, MatchFinder, Mode};
        let mut opts = LzmaOptions::new_preset(6).map_err(lz_err)?;
        opts.dict_size(self.dict_size)
            .literal_context_bits(self.lc)
            .literal_position_bits(self.lp)
            .position_bits(self.pb)
            .mode(if self.fast { Mode::Fast } else { Mode::Normal })
            .nice_len(self.nice_len)
            .match_finder(if self.hash_chain {
                MatchFinder::HashChain4
            } else {
                MatchFinder::BinaryTree4
            })
            .depth(self.depth);
        let mut filters = Filters::new();
        filters.lzma2(&opts);
        Stream::new_raw_encoder(&filters).map_err(lz_err)
    }
}

/// 单线程 LZMA2 编码：liblzma raw 编码器套在 Write 上，`finish` 写结束标记并交还底层 writer。
#[cfg(feature = "compress")]
pub(crate) struct Lzma2Encoder<W: std::io::Write> {
    inner: W,
    stream: Stream,
    /// 输出暂存，攒满或要刷时整块写到底层
    buf: Vec<u8>,
}

#[cfg(feature = "compress")]
impl<W: std::io::Write> Lzma2Encoder<W> {
    pub(crate) fn new(inner: W, options: &lzma_rust2::LzmaOptions) -> io::Result<Self> {
        let stream = Lzma2Params::from_options(options).raw_encoder()?;
        Ok(Self {
            inner,
            stream,
            buf: Vec::with_capacity(OUT_BUF_SIZE),
        })
    }

    /// 把暂存的输出写到底层。
    fn dump(&mut self) -> io::Result<()> {
        if !self.buf.is_empty() {
            self.inner.write_all(&self.buf)?;
            self.buf.clear();
        }
        Ok(())
    }

    /// 喂空输入、用同一个动作驱动到 StreamEnd（liblzma 要求 flush/finish 动作一旦开始就得用到结束）。
    fn drive(&mut self, action: Action) -> io::Result<()> {
        loop {
            self.dump()?;
            if self
                .stream
                .process_vec(&[], &mut self.buf, action)
                .map_err(lz_err)?
                == Status::StreamEnd
            {
                break;
            }
        }
        self.dump()
    }

    /// 写完所有数据和 0x00 结束标记，交还底层 writer。
    pub(crate) fn finish(mut self) -> io::Result<W> {
        self.drive(Action::Finish)?;
        self.inner.flush()?;
        Ok(self.inner)
    }
}

#[cfg(feature = "compress")]
impl<W: std::io::Write> std::io::Write for Lzma2Encoder<W> {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        if data.is_empty() {
            return Ok(0);
        }
        // 暂存区满了就先写出去再喂；一次没吃进任何输入就再来一轮
        loop {
            self.dump()?;
            let before = self.stream.total_in();
            self.stream
                .process_vec(data, &mut self.buf, Action::Run)
                .map_err(lz_err)?;
            let consumed = (self.stream.total_in() - before) as usize;
            if consumed > 0 {
                return Ok(consumed);
            }
        }
    }

    /// LZMA2 支持同步刷新：把已喂的数据封成完整 chunk 写出去，不结束流。
    fn flush(&mut self) -> io::Result<()> {
        self.drive(Action::SyncFlush)?;
        self.inner.flush()
    }
}

/// 多线程 LZMA2 编码：输入按 chunk_size 切块并行编码，每块独立成流，拼接时去掉块尾的结束标记。
#[cfg(feature = "compress")]
pub(crate) struct Lzma2EncoderMt<W: std::io::Write> {
    inner: W,
    pool: OrderedPool<Vec<u8>>,
    chunk_size: usize,
    /// 正在攒的块
    buf: Vec<u8>,
}

#[cfg(feature = "compress")]
impl<W: std::io::Write> Lzma2EncoderMt<W> {
    /// `options.chunk_size` 必须设置（与上游 Lzma2WriterMt 的要求一致），实际块大小不小于字典。
    pub(crate) fn new(
        inner: W,
        options: &lzma_rust2::Lzma2Options,
        threads: u32,
    ) -> io::Result<Self> {
        let chunk_size = match options.chunk_size {
            None => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "chunk size must be set",
                ));
            }
            Some(size) => size.get().max(u64::from(options.lzma_options.dict_size)),
        };
        let chunk_size = usize::try_from(chunk_size).map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidInput, "chunk size bigger than usize")
        })?;
        let params = Lzma2Params::from_options(&options.lzma_options);
        // 先建一个编码器确认参数合法，省得错误要等到线程里才冒出来
        params.raw_encoder()?;
        Ok(Self {
            inner,
            pool: OrderedPool::new(threads, move |data: Vec<u8>| encode_unit(&params, &data)),
            chunk_size,
            buf: Vec::new(),
        })
    }

    /// 写一个块的压缩结果：去掉末尾的 0x00 结束标记，整条流只在 finish 时补一个。
    fn write_unit(&mut self, out: Vec<u8>) -> io::Result<()> {
        match out.split_last() {
            Some((0x00, body)) => self.inner.write_all(body),
            _ => Err(io::Error::other(
                "liblzma LZMA2 unit did not end with the end marker",
            )),
        }
    }

    /// 把攒好的块派出去。派发前先写出已完成的结果；在飞的块够多了就等一个，控制内存。
    fn send_unit(&mut self) -> io::Result<()> {
        if self.buf.is_empty() {
            return Ok(());
        }
        while let Some(out) = self.pool.take_next(false)? {
            self.write_unit(out)?;
        }
        while self.pool.in_flight() >= self.pool.max_threads() {
            match self.pool.take_next(true)? {
                Some(out) => self.write_unit(out)?,
                None => break,
            }
        }
        let unit = std::mem::take(&mut self.buf);
        self.pool.dispatch(unit)
    }

    /// 等所有在飞的块写完。
    fn drain(&mut self) -> io::Result<()> {
        while let Some(out) = self.pool.take_next(true)? {
            self.write_unit(out)?;
        }
        Ok(())
    }

    /// 写完所有块和整条流的 0x00 结束标记，交还底层 writer。
    pub(crate) fn finish(mut self) -> io::Result<W> {
        self.send_unit()?;
        self.drain()?;
        self.inner.write_all(&[0x00])?;
        self.inner.flush()?;
        Ok(self.inner)
    }
}

#[cfg(feature = "compress")]
impl<W: std::io::Write> std::io::Write for Lzma2EncoderMt<W> {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        if data.is_empty() {
            return Ok(0);
        }
        if self.buf.capacity() == 0 {
            self.buf.reserve_exact(self.chunk_size);
        }
        let n = data.len().min(self.chunk_size - self.buf.len());
        self.buf.extend_from_slice(&data[..n]);
        if self.buf.len() == self.chunk_size {
            self.send_unit()?;
        }
        Ok(n)
    }

    /// 与上游 Lzma2WriterMt 一致：半截块也派出去编码，等全部写完再刷底层。
    fn flush(&mut self) -> io::Result<()> {
        self.send_unit()?;
        self.drain()?;
        self.inner.flush()
    }
}

/// 把一个块编码成独立的 LZMA2 流（含块尾的 0x00 结束标记）。
#[cfg(feature = "compress")]
fn encode_unit(params: &Lzma2Params, data: &[u8]) -> io::Result<Vec<u8>> {
    let mut stream = params.raw_encoder()?;
    // LZMA2 对不可压缩数据用「未压缩 chunk」存，最坏膨胀不到千分之一；按上限预留就不用中途扩容
    let mut out = Vec::with_capacity(data.len() + data.len() / 512 + OUT_BUF_SIZE);
    let mut input = data;
    loop {
        if out.capacity() - out.len() < 1024 {
            out.reserve(OUT_BUF_SIZE);
        }
        let before = stream.total_in();
        let action = if input.is_empty() {
            Action::Finish
        } else {
            Action::Run
        };
        let status = stream
            .process_vec(input, &mut out, action)
            .map_err(lz_err)?;
        input = &input[(stream.total_in() - before) as usize..];
        if status == Status::StreamEnd {
            return Ok(out);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::Read;
    #[cfg(feature = "compress")]
    use std::io::Write;

    use super::*;

    /// 拼一份既有重复又有噪声的数据，能压但压不到零。
    fn sample(len: usize) -> Vec<u8> {
        let mut state = 0x2545_F491_4F6C_DD1Du64;
        (0..len)
            .map(|i| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                if i % 7 == 0 {
                    (state & 0xFF) as u8
                } else {
                    (i % 251) as u8
                }
            })
            .collect()
    }

    fn decode_all(encoded: &[u8], dict_size: u32, threads: u32) -> Vec<u8> {
        let mut out = Vec::new();
        if threads < 2 {
            Lzma2Decoder::new(encoded, dict_size)
                .unwrap()
                .read_to_end(&mut out)
                .unwrap();
        } else {
            Lzma2DecoderMt::new(encoded, dict_size, threads)
                .unwrap()
                .read_to_end(&mut out)
                .unwrap();
        }
        out
    }

    #[cfg(feature = "compress")]
    fn mt_options(dict_size: u32, chunk_size: u64) -> lzma_rust2::Lzma2Options {
        let mut options = lzma_rust2::Lzma2Options::with_preset(3);
        options.lzma_options.dict_size = dict_size;
        options.set_chunk_size(std::num::NonZeroU64::new(chunk_size));
        options
    }

    #[cfg(feature = "compress")]
    #[test]
    fn single_thread_roundtrip() {
        let data = sample(300 * 1024);
        let options = lzma_rust2::Lzma2Options::with_preset(3);
        let mut encoder = Lzma2Encoder::new(Vec::new(), &options.lzma_options).unwrap();
        encoder.write_all(&data).unwrap();
        encoder.flush().unwrap();
        let encoded = encoder.finish().unwrap();
        assert_eq!(encoded.last(), Some(&0x00));
        assert_eq!(
            decode_all(&encoded, options.lzma_options.dict_size, 1),
            data
        );
        assert_eq!(
            decode_all(&encoded, options.lzma_options.dict_size, 4),
            data
        );
    }

    #[cfg(feature = "compress")]
    #[test]
    fn multi_thread_roundtrip_and_chunk_boundaries() {
        let data = sample(1_000_000);
        let options = mt_options(1 << 16, 200_000);
        let mut encoder = Lzma2EncoderMt::new(Vec::new(), &options, 3).unwrap();
        for piece in data.chunks(77_777) {
            encoder.write_all(piece).unwrap();
        }
        let encoded = encoder.finish().unwrap();
        // 5 块：每块起点都是字典重置 chunk，整条流只有最后一个 0x00
        assert_eq!(encoded.last(), Some(&0x00));
        assert!(encoded[0] >= 0xE0 || encoded[0] == 0x01);
        assert_eq!(decode_all(&encoded, 1 << 16, 1), data);
        assert_eq!(decode_all(&encoded, 1 << 16, 8), data);
    }

    #[cfg(feature = "compress")]
    #[test]
    fn empty_and_tiny_inputs() {
        let options = mt_options(1 << 16, 1 << 16);
        let encoded = Lzma2EncoderMt::new(Vec::new(), &options, 2)
            .unwrap()
            .finish()
            .unwrap();
        assert_eq!(encoded, vec![0x00]);
        assert!(decode_all(&encoded, 1 << 16, 2).is_empty());

        let mut encoder = Lzma2EncoderMt::new(Vec::new(), &options, 2).unwrap();
        encoder.write_all(b"x").unwrap();
        let encoded = encoder.finish().unwrap();
        assert_eq!(decode_all(&encoded, 1 << 16, 2), b"x");
    }

    #[test]
    fn truncated_stream_is_an_error() {
        let stream = [0xE0u8, 0x00, 0x05, 0x00, 0x08, 0x5D, 0x00, 0x1A, 0x44];
        assert!(
            Lzma2DecoderMt::new(&stream[..], 1 << 16, 2)
                .unwrap()
                .read_to_end(&mut Vec::new())
                .is_err()
        );
        assert!(
            Lzma2Decoder::new(&stream[..], 1 << 16)
                .unwrap()
                .read_to_end(&mut Vec::new())
                .is_err()
        );
        assert!(
            Lzma2DecoderMt::new(&[0x7Fu8][..], 1 << 16, 2)
                .unwrap()
                .read_to_end(&mut Vec::new())
                .is_err()
        );
    }
}
