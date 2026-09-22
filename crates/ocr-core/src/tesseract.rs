//! Tesseract 第二引擎（09 §9.2 T-B4-11，§9.0 OCR-①）：本地命令行子进程，非 FFI。
//!
//! 为什么是子进程：tesseract 的 FFI 绑定需与本机 DLL 版本严格同档，且随包分发 DLL
//! 属 B9 构件治理（[收窄] 登记见 DECISIONS）。用户自装 tesseract 后在设置里填绝对路径
//! 即可用，默认关。
//!
//! 安全面（本行引入"按用户配置路径执行外部二进制"，四层防线各一测试）：
//! ① 默认关（不启用就不进注册表，`ocr_engine_status` 里也不出现它）；
//! ② 路径必须绝对（不吃相对路径与裸程序名，防 PATH 劫持）；
//! ③ 参数以数组交 [`CommandRunner`]，全程零 shell、零字符串拼接；
//! ④ 超时强杀（[`SystemRunner`] 的 try_wait 轮询 + kill）。

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::{Mutex, RwLock};
use serde::{Deserialize, Serialize};

use host_core::error::AppError;
use host_core::ports::{Frame, OcrLine, Rect};

use crate::engine::OcrEngine;

/// 引擎 id（注册表项 / 设置词表 enum / 重建时的增删判据三处共用同一串）
pub const TESSERACT_ID: &str = "tesseract";

/// 语言探测缓存寿命：`--list-langs` 每次都要起一个进程，识别热路径不该重复付
const LANGS_TTL: Duration = Duration::from_secs(600);

/// 版式模式 6 = 统一文本块（截图取字的常见档）
const PSM: u8 = 6;

/// TSV 输出：官方 Command-Line-Usage 文档只写 `-c VAR=VAL`（`--config` 长写未见文档），
/// 且用内联变量而非位置参数 `tsv`——后者要求用户 tessdata 下存在 `configs/tsv`，
/// 装了语言包不等于装了配置档
const TSV_CONFIG_ARGS: [&str; 2] = ["-c", "tessedit_create_tsv=1"];

/// 一次子进程调用的结果（刻意不返回 `std::process::Output`：其 `ExitStatus` 无公开构造口，
/// 测试若拿它就必须在 crate 内再开一处进程入口，与本行"单一进程入口"红线冲突）
#[derive(Debug, Clone)]
pub struct CmdOutput {
    /// 退出码 0
    pub ok: bool,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

/// 子进程执行口（测试注 FakeRunner，全部断言不依赖本机是否装了 tesseract）
pub trait CommandRunner: Send + Sync {
    fn run(
        &self,
        exe: &OsStr,
        args: &[OsString],
        cwd: &Path,
        timeout_ms: u64,
    ) -> Result<CmdOutput, AppError>;
}

/// CREATE_NO_WINDOW：识别是后台动作，不该闪控制台窗
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// 真执行器：`Stdio::piped` + 读线程 + try_wait 轮询超时强杀
/// （口径同 win-integration maintenance.rs：管道自己读走，否则 try_wait 之后
/// `wait_with_output` 不可用——标准坑）
pub struct SystemRunner;

impl CommandRunner for SystemRunner {
    fn run(
        &self,
        exe: &OsStr,
        args: &[OsString],
        cwd: &Path,
        timeout_ms: u64,
    ) -> Result<CmdOutput, AppError> {
        let mut cmd = Command::new(exe);
        cmd.args(args)
            .current_dir(cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }
        let mut child = cmd.spawn().map_err(|e| {
            tess_err(
                &format!("启动 {} 失败: {e}", Path::new(exe).display()),
                Some("在设置里核对 Tesseract 可执行文件路径是否正确"),
            )
        })?;
        let mut out_pipe = child.stdout.take();
        let mut err_pipe = child.stderr.take();
        let out_reader = std::thread::spawn(move || {
            let mut buf = Vec::new();
            if let Some(p) = &mut out_pipe {
                use std::io::Read;
                let _ = p.read_to_end(&mut buf);
            }
            buf
        });
        let err_reader = std::thread::spawn(move || {
            let mut buf = Vec::new();
            if let Some(p) = &mut err_pipe {
                use std::io::Read;
                let _ = p.read_to_end(&mut buf);
            }
            buf
        });
        let deadline = Instant::now() + Duration::from_millis(timeout_ms);
        let status = loop {
            match child.try_wait() {
                Ok(Some(st)) => break st,
                Ok(None) => {
                    if Instant::now() > deadline {
                        let _ = child.kill();
                        let _ = child.wait();
                        // 读线程随管道关闭收尾；join 防句柄泄漏
                        let _ = out_reader.join();
                        let _ = err_reader.join();
                        return Err(tess_err(
                            &format!("Tesseract 执行超时（{timeout_ms}ms，已终止）"),
                            Some("可在设置里调大超时或缩小选区"),
                        ));
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
                Err(e) => return Err(tess_err(&format!("Tesseract 等待失败: {e}"), None)),
            }
        };
        let stdout = out_reader.join().unwrap_or_default();
        let stderr = err_reader.join().unwrap_or_default();
        Ok(CmdOutput {
            ok: status.success(),
            stdout,
            stderr,
        })
    }
}

fn tess_err(msg: &str, hint: Option<&str>) -> AppError {
    AppError::module("OCR_TESS_001", msg.to_owned(), hint)
}

fn tess_io(code: &str, msg: &str) -> AppError {
    AppError::module(code, msg.to_owned(), None)
}

/// Tesseract 四键（写侧仍只有 host_config_set 一个口；本结构是运行态投影）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct TesseractSettings {
    pub enabled: bool,
    pub exe: String,
    pub data_dir: Option<String>,
    pub timeout_ms: u64,
}

impl Default for TesseractSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            exe: String::new(),
            data_dir: None,
            timeout_ms: 20_000,
        }
    }
}

impl TesseractSettings {
    /// tessdata 目录：空串按"未填"处理（设置面的 Input 清值只会留空串，不会回填 null）
    pub fn tessdata_dir(&self) -> Option<&str> {
        self.data_dir
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
    }

    /// 启用前置校验：关着的路径值不产生任何行为，故不校验；一旦启用就必须是绝对路径
    pub fn validate(&self) -> Result<(), AppError> {
        if !self.enabled {
            return Ok(());
        }
        if self.exe.trim().is_empty() {
            return Err(tess_err(
                "启用 Tesseract 需填可执行文件绝对路径",
                Some("例：C:\\Program Files\\Tesseract-OCR\\tesseract.exe"),
            ));
        }
        if !Path::new(self.exe.trim()).is_absolute() {
            return Err(tess_err(
                &format!(
                    "Tesseract 可执行文件必须是绝对路径，当前值：{}",
                    self.exe.trim()
                ),
                Some("不吃相对路径与裸程序名：那会走 PATH 解析，等于给任意同名程序开门"),
            ));
        }
        Ok(())
    }
}

/// tesseract CLI 引擎项：设置活在 `Arc<RwLock>` 里，引擎实例随注册表按设置重建
pub struct TesseractEngine {
    settings: Arc<RwLock<TesseractSettings>>,
    runner: Arc<dyn CommandRunner>,
    /// 临时帧根目录（{app_data}）：识别期在其下 `ocr-tmp/` 写一次性 PNG
    app_data: PathBuf,
    langs_cache: Mutex<Option<(Instant, Vec<String>)>>,
}

impl TesseractEngine {
    pub fn new(
        settings: Arc<RwLock<TesseractSettings>>,
        runner: Arc<dyn CommandRunner>,
        app_data: PathBuf,
    ) -> Self {
        Self {
            settings,
            runner,
            app_data,
            langs_cache: Mutex::new(None),
        }
    }

    /// 已校验且确实存在的可执行文件路径
    fn exe_path(&self) -> Result<PathBuf, AppError> {
        let s = self.settings.read().clone();
        s.validate()?;
        let path = PathBuf::from(s.exe.trim());
        if !path.is_file() {
            return Err(tess_err(
                &format!("Tesseract 可执行文件不存在：{}", path.display()),
                Some("安装 tesseract 后在设置里填可执行文件绝对路径"),
            ));
        }
        Ok(path)
    }

    fn run(&self, args: &[OsString], cwd: &Path) -> Result<CmdOutput, AppError> {
        let s = self.settings.read();
        let exe = OsString::from(s.exe.trim());
        let timeout_ms = s.timeout_ms;
        drop(s);
        let out = self.runner.run(&exe, args, cwd, timeout_ms)?;
        if !out.ok {
            return Err(tess_err(
                &format!(
                    "tesseract 非零退出：{}",
                    String::from_utf8_lossy(&out.stderr).trim()
                ),
                Some("多为语言包缺失：核对 tessdata 目录与 -l 语言名"),
            ));
        }
        Ok(out)
    }
}

impl OcrEngine for TesseractEngine {
    fn id(&self) -> &'static str {
        TESSERACT_ID
    }

    fn display_name(&self) -> &'static str {
        "Tesseract（本地命令行）"
    }

    fn available(&self) -> Result<Vec<String>, AppError> {
        if !self.settings.read().enabled {
            return Err(tess_err(
                "Tesseract 引擎未启用",
                Some("在设置里勾选启用并填写可执行文件绝对路径"),
            ));
        }
        let exe = self.exe_path()?;
        if let Some((at, langs)) = self.langs_cache.lock().as_ref() {
            if at.elapsed() < LANGS_TTL {
                return Ok(langs.clone());
            }
        }
        // cwd 取 exe 所在目录：tesseract 在 Windows 上按自身目录找 tessdata 缺省档
        let cwd = exe.parent().unwrap_or_else(|| Path::new("."));
        let out = self.run(&[os("--list-langs")], cwd)?;
        let langs = parse_langs(&String::from_utf8_lossy(&out.stdout));
        *self.langs_cache.lock() = Some((Instant::now(), langs.clone()));
        Ok(langs)
    }

    /// 临时 PNG → CLI（TSV 到 stdout）→ 归一化行；临时帧成败都删
    fn recognize(&self, frame: &Frame, lang: &str) -> Result<Vec<OcrLine>, AppError> {
        let s = self.settings.read().clone();
        s.validate()?;
        let dir = self.app_data.join("ocr-tmp");
        std::fs::create_dir_all(&dir)
            .map_err(|e| tess_io("OCR_TESS_003", &format!("创建临时帧目录失败: {e}")))?;
        let name = uuid::Uuid::now_v7().simple().to_string();
        let path = dir.join(format!("{name}.png"));
        let tmp = dir.join(format!("{name}.png.tmp"));
        std::fs::write(&tmp, encode_png(frame)?)
            .map_err(|e| tess_io("OCR_TESS_003", &format!("写临时帧失败: {e}")))?;
        if let Err(e) = std::fs::rename(&tmp, &path) {
            let _ = std::fs::remove_file(&tmp);
            return Err(tess_io("OCR_TESS_003", &format!("临时帧落盘失败: {e}")));
        }
        let args = build_args(&path.to_string_lossy(), lang, s.tessdata_dir(), PSM);
        let outcome = self.run(&args, &dir);
        // 成败都删：识别失败也不该在 {app_data}/ocr-tmp 里堆图
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(&tmp);
        let out = outcome?;
        Ok(parse_tsv(
            &String::from_utf8_lossy(&out.stdout),
            frame.width,
            frame.height,
        ))
    }
}

fn os(s: &str) -> OsString {
    OsString::from(s)
}

/// 参数数组（禁字符串拼接：注入型语言名也必须整体占一个元素）
///
/// lang 为空 = 用户配置的语言一个都没装在本机 → 不传 `-l`（tesseract 拒空语言名，
/// 而它自己的默认语言就是它的"自选"档）。
pub fn build_args(in_path: &str, lang: &str, data_dir: Option<&str>, psm: u8) -> Vec<OsString> {
    let mut args: Vec<OsString> = vec![os(in_path), os("stdout")];
    if !lang.is_empty() {
        args.push(os("-l"));
        args.push(os(lang));
    }
    args.push(os("--psm"));
    args.push(psm.to_string().into());
    if let Some(dir) = data_dir {
        args.push(os("--tessdata-dir"));
        args.push(os(dir));
    }
    args.extend(TSV_CONFIG_ARGS.iter().map(|a| os(a)));
    args
}

/// `--list-langs` 输出 → 语言名列表：首行是 `List of available languages in "…" (3):`
/// 说明行，语言名是裸 token（`eng` / `chi_sim` / `osd`），故按"含空白即非语言名"剥除
pub fn parse_langs(text: &str) -> Vec<String> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.chars().any(char::is_whitespace))
        .filter(|l| {
            l.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
        })
        .map(str::to_owned)
        .collect()
}

/// TSV（`level page_num block_num par_num line_num word_num left top width height conf text`）
/// → 归一化 `OcrLine`：只取词级（level 5）行按 (block, par, line) 聚行，坐标按页尺寸归一，
/// 畸形行（列数不足 / 数字列非数字）静默跳过——引擎局部坏行不等于整次识别作废
pub fn parse_tsv(text: &str, page_w: u32, page_h: u32) -> Vec<OcrLine> {
    if page_w == 0 || page_h == 0 {
        return vec![];
    }
    let mut acc = LineAcc::default();
    let mut out: Vec<OcrLine> = Vec::new();
    for raw in text.lines() {
        let cols: Vec<&str> = raw.split('\t').collect();
        if cols.len() < 12 || cols[0].trim() != "5" {
            continue; // 表头 / 页块段行级汇总行（text 恒空）/ 畸形短行
        }
        let key = match (
            cols[2].trim().parse::<i64>(),
            cols[3].trim().parse::<i64>(),
            cols[4].trim().parse::<i64>(),
        ) {
            (Ok(b), Ok(p), Ok(l)) => (b, p, l),
            _ => continue,
        };
        let word = cols[11].trim();
        if word.is_empty() || word == "-1" {
            continue;
        }
        let geom = match (
            cols[6].trim().parse::<i64>(),
            cols[7].trim().parse::<i64>(),
            cols[8].trim().parse::<i64>(),
            cols[9].trim().parse::<i64>(),
        ) {
            (Ok(l), Ok(t), Ok(w), Ok(h)) => (l, t, w, h),
            _ => (0, 0, 0, 0),
        };
        let conf = cols[10].trim().parse::<f64>().unwrap_or(-1.0);
        if acc.key != Some(key) {
            acc.flush(&mut out, page_w, page_h);
            acc.key = Some(key);
        }
        acc.push(word, geom, conf);
    }
    acc.flush(&mut out, page_w, page_h);
    out
}

#[derive(Default)]
struct LineAcc {
    key: Option<(i64, i64, i64)>,
    words: Vec<String>,
    min: (i64, i64),
    max: (i64, i64),
    conf_sum: f64,
    conf_n: usize,
}

impl LineAcc {
    fn push(&mut self, word: &str, geom: (i64, i64, i64, i64), conf: f64) {
        let (l, t, w, h) = geom;
        if self.words.is_empty() {
            self.min = (l, t);
            self.max = (l + w, t + h);
        } else {
            self.min = (self.min.0.min(l), self.min.1.min(t));
            self.max = (self.max.0.max(l + w), self.max.1.max(t + h));
        }
        self.words.push(word.to_owned());
        if conf >= 0.0 {
            self.conf_sum += conf;
            self.conf_n += 1;
        }
    }

    fn flush(&mut self, out: &mut Vec<OcrLine>, page_w: u32, page_h: u32) {
        if self.words.is_empty() {
            self.key = None;
            return;
        }
        // 词级 conf 全为 -1（引擎确实没给分）→ 0.0，与"低置信"区分留给 T-B4-13 的诚实化面
        let confidence = if self.conf_n == 0 {
            0.0
        } else {
            (self.conf_sum / self.conf_n as f64 / 100.0).clamp(0.0, 1.0) as f32
        };
        let x = self.min.0.max(0) as f32 / page_w as f32;
        let y = self.min.1.max(0) as f32 / page_h as f32;
        let w = (self.max.0 - self.min.0).max(0) as f32 / page_w as f32;
        let h = (self.max.1 - self.min.1).max(0) as f32 / page_h as f32;
        out.push(OcrLine {
            text: std::mem::take(&mut self.words).join(" "),
            rect: Rect { x, y, w, h },
            confidence,
        });
        self.key = None;
        self.min = (0, 0);
        self.max = (0, 0);
        self.conf_sum = 0.0;
        self.conf_n = 0;
    }
}

fn encode_png(frame: &Frame) -> Result<Vec<u8>, AppError> {
    use image::RgbaImage;
    let rgba = bgra_to_rgba(frame);
    let img = RgbaImage::from_raw(frame.width, frame.height, rgba)
        .ok_or_else(|| tess_io("OCR_TESS_003", "帧尺寸与缓冲不匹配（内部错误）"))?;
    let mut buf = std::io::Cursor::new(Vec::new());
    img.write_to(&mut buf, image::ImageFormat::Png)
        .map_err(|e| tess_io("OCR_TESS_003", &format!("PNG 编码失败: {e}")))?;
    Ok(buf.into_inner())
}

/// BGRA 帧 → RGBA（`OcrEngine` 契约是 BGRA、alpha 无意义故置 255；与 screenshot-core
/// 同口径，模块间不互依赖所以各留一份）
fn bgra_to_rgba(frame: &Frame) -> Vec<u8> {
    let pixels = frame.width as usize * frame.height as usize;
    let src = frame.bgra.as_ref();
    let mut out = vec![255u8; pixels * 4];
    for i in 0..pixels.min(src.len() / 4) {
        out[i * 4] = src[i * 4 + 2];
        out[i * 4 + 1] = src[i * 4 + 1];
        out[i * 4 + 2] = src[i * 4];
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex as StdMutex;

    /// 假执行器：记录每次收到的参数数组，按脚本返回固定 stdout 或 Err（=超时/启动失败臂）
    struct FakeRunner {
        stdout: String,
        fail: bool,
        calls: Arc<StdMutex<Vec<Vec<OsString>>>>,
        spawns: Arc<AtomicUsize>,
    }

    impl FakeRunner {
        fn new(stdout: &str) -> Self {
            Self {
                stdout: stdout.to_owned(),
                fail: false,
                calls: Arc::new(StdMutex::new(Vec::new())),
                spawns: Arc::new(AtomicUsize::new(0)),
            }
        }
    }

    impl CommandRunner for FakeRunner {
        fn run(
            &self,
            _exe: &OsStr,
            args: &[OsString],
            _cwd: &Path,
            _timeout_ms: u64,
        ) -> Result<CmdOutput, AppError> {
            self.spawns.fetch_add(1, Ordering::SeqCst);
            self.calls.lock().unwrap().push(args.to_vec());
            if self.fail {
                return Err(tess_err("Tesseract 执行超时（5000ms，已终止）", None));
            }
            Ok(CmdOutput {
                ok: true,
                stdout: self.stdout.as_bytes().to_vec(),
                stderr: Vec::new(),
            })
        }
    }

    /// 注册表里的"其余引擎"占位项（证明 tesseract 的增删不牵连既有项）
    struct StandInEngine;
    impl OcrEngine for StandInEngine {
        fn id(&self) -> &'static str {
            "stand-in"
        }
        fn display_name(&self) -> &'static str {
            "Stand In"
        }
        fn available(&self) -> Result<Vec<String>, AppError> {
            Ok(vec!["zh-CN".into()])
        }
        fn recognize(&self, _frame: &Frame, _lang: &str) -> Result<Vec<OcrLine>, AppError> {
            Ok(vec![])
        }
    }

    fn frame_2x2() -> Frame {
        Frame {
            width: 2,
            height: 2,
            bgra: Arc::from([10u8, 20, 30, 255].as_slice().repeat(4)),
            dpi_scale: 1.0,
            monitor_id: 0,
        }
    }

    const LANGS_OUT: &str = "List of available languages in \"C:/tess\" (3):\neng\nosd\nchi_sim\n";
    const TSV_TWO: &str = "level\tpage_num\tblock_num\tpar_num\tline_num\tword_num\tleft\ttop\twidth\theight\tconf\ttext\n\
                           5\t1\t1\t1\t1\t1\t0\t0\t1\t1\t90\t你好\n";

    fn enabled_settings(exe: &str) -> TesseractSettings {
        TesseractSettings {
            enabled: true,
            exe: exe.to_owned(),
            data_dir: Some("C:/tessdata".into()),
            timeout_ms: 5_000,
        }
    }

    /// 假执行器的调用日志（clippy type_complexity：嵌套 Arc/Mutex/Vec 收成一名）
    type CallLog = Arc<StdMutex<Vec<Vec<OsString>>>>;

    fn engine_with(
        dir: &Path,
        exe: &str,
        runner: FakeRunner,
    ) -> (TesseractEngine, CallLog, Arc<AtomicUsize>) {
        let calls = runner.calls.clone();
        let spawns = runner.spawns.clone();
        let eng = TesseractEngine::new(
            Arc::new(RwLock::new(enabled_settings(exe))),
            Arc::new(runner),
            dir.to_path_buf(),
        );
        (eng, calls, spawns)
    }

    fn plain(args: &[OsString]) -> Vec<String> {
        args.iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    #[allow(non_snake_case)] // 任务书（09 §9.2 T-B4-11）字面测试名优先于 rustc 命名惯例
    fn tesseractArgs_languageWithShellMetas_staysSingleArg() {
        let args = plain(&build_args(
            r"C:\tmp\x.png",
            "zh-CN; rm -rf /",
            Some(r"C:\tess"),
            6,
        ));
        assert_eq!(
            args,
            vec![
                r"C:\tmp\x.png",
                "stdout",
                "-l",
                "zh-CN; rm -rf /",
                "--psm",
                "6",
                "--tessdata-dir",
                r"C:\tess",
                "-c",
                "tessedit_create_tsv=1"
            ],
            "注入型语言名必须恰占一个元素，且全程无 shell 入口"
        );
        assert!(
            !args
                .iter()
                .any(|a| matches!(a.as_str(), "cmd" | "/c" | "sh" | "-exec" | "/bin/bash")),
            "参数序列里不得出现 shell 入口：{args:?}"
        );
        // 空语言 = 不传 -l（tesseract 拒空语言名，其默认语言即它的自选档）
        assert_eq!(
            plain(&build_args("x.png", "", None, 6)),
            vec![
                "x.png",
                "stdout",
                "--psm",
                "6",
                "-c",
                "tessedit_create_tsv=1"
            ]
        );
    }

    #[test]
    #[allow(non_snake_case)]
    fn tesseractDisabled_neverRegisteredAndStatusListsOne() {
        let dir = tempfile::tempdir().unwrap();
        let off = TesseractSettings::default();
        assert!(!off.enabled, "红线：默认关——不勾选就不该有第二条引擎路径");
        off.validate().unwrap();
        // 相对路径的假 exe（真实存在）：启用必进注册表，禁用则连"不可用"都不该出现
        let fake = dir.path().join("tesseract-fake.exe");
        std::fs::write(&fake, b"not a real exe").unwrap();
        let runner: Arc<dyn CommandRunner> = Arc::new(FakeRunner::new(LANGS_OUT));
        let base: Vec<Arc<dyn OcrEngine>> = vec![Arc::new(StandInEngine)];
        let on = TesseractSettings {
            enabled: true,
            exe: fake.to_string_lossy().into_owned(),
            data_dir: None,
            timeout_ms: 5_000,
        };
        let two = crate::module::build_registry(
            base.clone(),
            &on,
            runner.clone(),
            dir.path().to_path_buf(),
        );
        let ids: Vec<String> = two.status().into_iter().map(|e| e.id).collect();
        assert_eq!(
            ids,
            vec!["stand-in".to_string(), "tesseract".to_string()],
            "正对照：启用即进注册表"
        );
        let one = crate::module::build_registry(
            base,
            &TesseractSettings::default(),
            runner,
            dir.path().to_path_buf(),
        );
        let ids: Vec<String> = one.status().into_iter().map(|e| e.id).collect();
        assert_eq!(
            ids,
            vec!["stand-in".to_string()],
            "默认关：tesseract 不出现在引擎状态面"
        );
        assert_eq!(one.languages(), vec!["zh-CN".to_string()]);
    }

    #[test]
    #[allow(non_snake_case)]
    fn tesseractAvailable_relativePathOrMissing_rejects001NamingPath() {
        let dir = tempfile::tempdir().unwrap();
        let fake = dir.path().join("tesseract-fake.exe");
        std::fs::write(&fake, b"x").unwrap();
        // 臂一：相对路径拒且零 spawn（校验不过就不该起进程）
        let (eng, _, spawns) = engine_with(dir.path(), "tesseract.exe", FakeRunner::new(LANGS_OUT));
        let err = eng.available().unwrap_err().to_string();
        assert!(err.contains("绝对路径"), "{err}");
        assert_eq!(spawns.load(Ordering::SeqCst), 0);
        // 臂二：绝对路径但不存在 → 拒，且消息点名当前路径 + 给首动作
        // （首动作住 hint：前端 parseErr 显示 message + hint，故按上线的序列化形状断言）
        let missing = dir.path().join("nope.exe").to_string_lossy().into_owned();
        let (eng2, _, spawns2) = engine_with(dir.path(), &missing, FakeRunner::new(LANGS_OUT));
        let shown = serde_json::to_string(&eng2.available().unwrap_err()).unwrap();
        assert!(shown.contains("nope.exe"), "错误须点名路径：{shown}");
        assert!(shown.contains("安装 tesseract"), "须给首动作：{shown}");
        assert_eq!(spawns2.load(Ordering::SeqCst), 0);
        // 臂三：存在即探测，前导说明行剥除；未启用则同拒（注册表外的第三道闸）
        let (eng3, calls, spawns3) = engine_with(
            dir.path(),
            &fake.to_string_lossy(),
            FakeRunner::new(LANGS_OUT),
        );
        assert_eq!(
            eng3.available().unwrap(),
            vec!["eng", "osd", "chi_sim"],
            "--list-langs 的 List of available… 行须剥除"
        );
        assert_eq!(plain(&calls.lock().unwrap()[0]), vec!["--list-langs"]);
        let n = spawns3.load(Ordering::SeqCst);
        eng3.available().unwrap();
        assert_eq!(spawns3.load(Ordering::SeqCst), n, "600s TTL 内不重复起进程");
        let disabled = TesseractEngine::new(
            Arc::new(RwLock::new(TesseractSettings::default())),
            Arc::new(FakeRunner::new(LANGS_OUT)),
            dir.path().to_path_buf(),
        );
        assert!(
            disabled.available().is_err(),
            "关着的引擎即便被注册也不可用"
        );
    }

    #[test]
    #[allow(non_snake_case)]
    fn tesseractTsv_rowsNormalizedAndMalformedSkipped() {
        const HEADER: &str =
            "level\tpage_num\tblock_num\tpar_num\tline_num\tword_num\tleft\ttop\twidth\theight\tconf\ttext\n";
        let tsv = format!(
            "{HEADER}5\t1\t1\t1\t1\t1\t100\t200\t50\t20\t95.5\tHello\n\
             5\t1\t1\t1\t1\t2\t160\t200\t40\t20\t88.5\tOCR\n\
             5\t1\t1\t1\t2\t1\t100\t260\t60\t20\t-1\tSecond\n\
             5\t1\t2\t1\t1\t1\t1\t1\t1\t1\tabc\tnoConf\n\
             3\t1\t1\t0\t0\t0\t0\t0\t0\t0\t-1\t\n\
             短行\n\
             \n"
        );
        let lines = parse_tsv(&tsv, 1000, 500);
        assert_eq!(lines.len(), 3, "坏行静默跳过、词按行聚合：{lines:?}");
        assert_eq!(lines[0].text, "Hello OCR");
        assert!(
            (lines[0].confidence - 0.92).abs() < 0.001,
            "conf 均值 /100：{}",
            lines[0].confidence
        );
        assert_eq!((lines[0].rect.x, lines[0].rect.y), (0.1, 0.4));
        assert_eq!((lines[0].rect.w, lines[0].rect.h), (0.1, 0.04));
        assert_eq!(lines[1].text, "Second");
        assert_eq!(
            lines[1].confidence, 0.0,
            "conf=-1（引擎确实没给分）归 0.0 不谎报"
        );
        assert_eq!(lines[2].text, "noConf");
        assert_eq!(lines[2].confidence, 0.0, "conf 非数字同按未给分处理");
        assert!(parse_tsv(&tsv, 0, 0).is_empty(), "页尺寸 0 不得除零");
        // 语言表面（同批核证）：说明行与 Error 行都不进列表
        assert_eq!(parse_langs(LANGS_OUT), vec!["eng", "osd", "chi_sim"]);
        assert_eq!(
            parse_langs("Error: no tessdata found\n"),
            Vec::<String>::new()
        );
    }

    #[test]
    #[allow(non_snake_case)]
    fn tesseractTempPng_removedOnSuccessAndFailure() {
        let dir = tempfile::tempdir().unwrap();
        let fake = dir.path().join("tesseract-fake.exe");
        std::fs::write(&fake, b"x").unwrap();
        let tmp = dir.path().join("ocr-tmp");
        let leftovers = |d: &Path| -> Vec<String> {
            std::fs::read_dir(d)
                .map(|rd| {
                    rd.filter_map(Result::ok)
                        .map(|e| e.file_name().to_string_lossy().into_owned())
                        .collect()
                })
                .unwrap_or_default()
        };
        let (eng, calls, _) = engine_with(
            dir.path(),
            &fake.to_string_lossy(),
            FakeRunner::new(TSV_TWO),
        );
        let got = eng.recognize(&frame_2x2(), "zh-CN").unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].text, "你好");
        assert_eq!(leftovers(&tmp), Vec::<String>::new(), "成功不留临时帧");
        // 识别输入是真 PNG（非裸帧）：首条参数序列的入参就是临时帧绝对路径
        let args = plain(&calls.lock().unwrap()[0]);
        assert!(args[0].ends_with(".png"), "输入应是 PNG：{args:?}");
        // 失败臂（runner 交 Err，等价超时/启动失败）同样不留残帧
        let mut f = FakeRunner::new("");
        f.fail = true;
        let (eng2, _, _) = engine_with(dir.path(), &fake.to_string_lossy(), f);
        let err = eng2.recognize(&frame_2x2(), "en").unwrap_err().to_string();
        assert!(err.contains("超时"), "runner 错误如实上抛：{err}");
        assert_eq!(leftovers(&tmp), Vec::<String>::new(), "失败路径也不留残帧");
    }

    #[test]
    #[allow(non_snake_case)]
    fn tesseractTimeout_killsAndMapsToErr() {
        let dir = tempfile::tempdir().unwrap();
        let fake = dir.path().join("tesseract-fake.exe");
        std::fs::write(&fake, b"x").unwrap();
        let mut f = FakeRunner::new("");
        f.fail = true;
        let (eng, calls, _) = engine_with(dir.path(), &fake.to_string_lossy(), f);
        let err = eng.recognize(&frame_2x2(), "en").unwrap_err();
        assert!(err.to_string().contains("超时"), "{err}");
        assert!(err.to_string().contains("已终止"), "须交代已经强杀：{err}");
        let args = plain(&calls.lock().unwrap()[0]);
        assert!(args.contains(&"-c".to_string()), "TSV 走 -c 内联：{args:?}");
        assert!(args.contains(&"tessedit_create_tsv=1".to_string()));
        // 真 SystemRunner 臂：不存在的 exe → 启动失败错误（OCR_TESS_001），不 panic；
        // "起了进程到点强杀"的时序路径要真二进制，归批次尾实启冒烟（人工）
        let out = SystemRunner.run(
            OsStr::new("no-such-tesseract-exe-xyz.exe"),
            &[],
            dir.path(),
            500,
        );
        let e = out.unwrap_err().to_string();
        assert!(e.contains("启动"), "不存在的 exe 走启动失败臂：{e}");
    }
}
