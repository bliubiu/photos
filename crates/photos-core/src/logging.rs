//! 日志系统：tracing + 文件日轮转 + 终端输出。
//!
//! 规范对齐：
//! - 格式：`[YYYY-MM-DD HH:MM:SS.SSS] [级别] [线程ID] [模块:行号] - 内容`
//! - 文件名：`photos-YYYYMMDD.log`，保留 32 天，启动时自动清理过期日志
//! - 内容中文；写入前统一脱敏（身份证/手机号/密码等，见 docs/06-安全设计.md）

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use chrono::{Local, NaiveDate};
use tracing::Event;
use tracing::field::{Field, Visit};
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::fmt::FmtContext;
use tracing_subscriber::fmt::format::{FormatEvent, FormatFields};
use tracing_subscriber::fmt::writer::MakeWriter;
use tracing_subscriber::registry::LookupSpan;

use crate::config::LogLevel;
use crate::error::{CoreError, CoreResult};
use crate::metrics::TaskMetrics;

/// 初始化全局日志：文件（日轮转）+ 终端双写，返回文件写入器的 worker guard（main 退出时刷盘）
pub fn init_logging(log_dir: &Path, level: LogLevel) -> CoreResult<WorkerGuard> {
    cleanup_old_logs(log_dir, 32)
        .map_err(|e| CoreError::Logging(format!("清理过期日志失败：{e}")))?;

    let file_writer = DailyFileWriter::new(log_dir.to_path_buf());
    let (file_nb, guard) = tracing_appender::non_blocking(file_writer);

    let subscriber = tracing_subscriber::fmt()
        .event_format(ChineseLogFormat)
        .with_writer(DualWriter { file: file_nb })
        .with_max_level(level.as_tracing())
        .finish();

    tracing::subscriber::set_global_default(subscriber)
        .map_err(|e| CoreError::Logging(format!("设置全局日志订阅器失败：{e}")))?;
    Ok(guard)
}

/// 双写写入器：同一事件同时写入文件（非阻塞）与终端
#[derive(Clone)]
struct DualWriter {
    file: tracing_appender::non_blocking::NonBlocking,
}

impl<'a> MakeWriter<'a> for DualWriter {
    type Writer = DualSink;

    fn make_writer(&'a self) -> Self::Writer {
        DualSink {
            file: self.file.clone(),
            stdout: io::stdout(),
        }
    }
}

/// 双写目标：文件 + 标准输出
struct DualSink {
    file: tracing_appender::non_blocking::NonBlocking,
    stdout: io::Stdout,
}

impl Write for DualSink {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let _ = self.stdout.write(buf)?;
        self.file.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.stdout.flush()?;
        self.file.flush()
    }
}

/// 每日轮转文件写入器：跨日自动切换 `photos-YYYYMMDD.log`
#[derive(Debug)]
pub struct DailyFileWriter {
    dir: PathBuf,
    today: NaiveDate,
    file: Option<File>,
}

impl DailyFileWriter {
    /// 新建写入器（惰性建文件，首次写入时创建目录与当日文件）
    pub fn new(dir: PathBuf) -> Self {
        Self {
            dir,
            today: Local::now().date_naive(),
            file: None,
        }
    }

    /// 确保打开当日文件（跨日时重建）
    fn ensure(&mut self) -> io::Result<&mut File> {
        let today = Local::now().date_naive();
        if today != self.today {
            self.file = None;
            self.today = today;
        }
        if self.file.is_none() {
            std::fs::create_dir_all(&self.dir)?;
            let path = self
                .dir
                .join(format!("photos-{}.log", self.today.format("%Y%m%d")));
            self.file = Some(OpenOptions::new().create(true).append(true).open(path)?);
        }
        Ok(self.file.as_mut().expect("文件句柄已创建"))
    }
}

impl Write for DailyFileWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.ensure()?.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        if let Some(f) = &mut self.file {
            f.flush()
        } else {
            Ok(())
        }
    }
}

/// 清理超过保留天数的日志文件（`photos-*.log`），返回被删除的文件列表
pub fn cleanup_old_logs(dir: &Path, keep_days: u32) -> io::Result<Vec<PathBuf>> {
    let cutoff = Local::now().date_naive() - chrono::Duration::days(keep_days as i64);
    let mut removed = Vec::new();
    if !dir.exists() {
        return Ok(removed);
    }
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()).map(String::from) else {
            continue;
        };
        let Some(stamp) = name
            .strip_prefix("photos-")
            .and_then(|r| r.strip_suffix(".log"))
        else {
            continue;
        };
        let Ok(date) = NaiveDate::parse_from_str(stamp, "%Y%m%d") else {
            continue;
        };
        if date < cutoff {
            std::fs::remove_file(&path)?;
            removed.push(path);
        }
    }
    Ok(removed)
}

/// 中文日志格式：`[时间] [级别] [线程] [模块:行号] - 内容`
pub struct ChineseLogFormat;

impl<S, N> FormatEvent<S, N> for ChineseLogFormat
where
    S: tracing::Subscriber + for<'a> LookupSpan<'a>,
    N: for<'a> FormatFields<'a> + 'static,
{
    fn format_event(
        &self,
        _ctx: &FmtContext<'_, S, N>,
        mut writer: tracing_subscriber::fmt::format::Writer<'_>,
        event: &Event<'_>,
    ) -> std::fmt::Result {
        let now = Local::now().format("%Y-%m-%d %H:%M:%S%.3f");
        let level = event.metadata().level();
        let tid = short_thread_id();
        let target = event.metadata().target();
        let line = event.metadata().line().unwrap_or(0);

        write!(writer, "[{now}] [{level}] [{tid}] [{target}:{line}] - ")?;

        let mut visitor = MessageVisitor::default();
        event.record(&mut visitor);
        let message = visitor.message.as_deref().unwrap_or("");
        let text = if visitor.fields.is_empty() {
            message.to_string()
        } else {
            format!("{message} {}", visitor.fields.join(" "))
        };
        // 整行（含结构化字段值）统一脱敏
        write!(writer, "{}", redact(&text))?;
        writeln!(writer)
    }
}

/// 事件字段收集器（提取 message 与其余字段）
///
/// 按字段类型分别实现 `Visit`：字符串不加引号、数值/布尔原样输出，
/// 便于结构化日志可读与机器解析（`record_debug` 仅作为兜底）。
#[derive(Default)]
struct MessageVisitor {
    message: Option<String>,
    fields: Vec<String>,
}

impl MessageVisitor {
    /// 记录一个字段（`message` 单独存放，其余按 `键=值` 追加）
    fn push(&mut self, field: &Field, rendered: String) {
        if field.name() == "message" {
            self.message = Some(rendered);
        } else {
            self.fields.push(format!("{}={rendered}", field.name()));
        }
    }
}

impl Visit for MessageVisitor {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        self.push(field, format!("{value:?}"));
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        self.push(field, value.to_string());
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        self.push(field, value.to_string());
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        self.push(field, value.to_string());
    }

    fn record_f64(&mut self, field: &Field, value: f64) {
        self.push(field, value.to_string());
    }

    fn record_bool(&mut self, field: &Field, value: bool) {
        self.push(field, value.to_string());
    }
}

/// 记录任务分阶段耗时（结构化字段 + 中文摘要；无指标时不输出）
pub fn log_metrics(task_id: i64, metrics: &TaskMetrics) {
    if metrics.is_empty() {
        return;
    }
    tracing::info!(
        任务 = task_id,
        总耗时毫秒 = metrics.total_ms().round() as i64,
        分阶段耗时 = metrics.summary(),
        "任务分阶段耗时统计"
    );
}

/// 记录结构化错误日志（错误码 / 阶段 / 任务 id；任务无关时 task_id 为 None）
pub fn log_error(code: &str, stage: &str, message: &str, task_id: Option<i64>) {
    tracing::error!(
        错误码 = code,
        阶段 = stage,
        任务 = ?task_id,
        "{message}"
    );
}

/// 线程短标识：`ThreadId(12)` → `T12`
fn short_thread_id() -> String {
    let d = format!("{:?}", std::thread::current().id());
    d.trim_start_matches("ThreadId(")
        .trim_end_matches(')')
        .to_string()
}

/// 敏感信息脱敏：身份证号、手机号、常见密钥字段值（见 docs/06-安全设计.md §3.3）
pub fn redact(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let bytes = input.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        // 密钥字段：key = 值（键名大小写不敏感）
        if let Some((key_end, val_end)) = secret_value_range(&input[i..]) {
            out.push_str(&input[i..i + key_end]);
            out.push_str("***");
            i += val_end;
            continue;
        }
        // 手机号：1[3-9] 后接 9 位数字
        if let Some(len) = mobile_len(&input[i..]) {
            out.push_str(&input[i..i + 3]);
            out.push_str("********");
            i += len;
            continue;
        }
        // 身份证：17 位数字 + 数字/X/x
        if let Some(len) = idcard_len(&input[i..]) {
            out.push_str(&input[i..i + 6]);
            out.push_str("********");
            out.push_str(&input[i + 14..i + len]);
            i += len;
            continue;
        }
        // 普通字符原样输出
        let ch = input[i..].chars().next().expect("非空字符");
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

const SECRET_KEYS: &[&str] = &[
    "password", "passwd", "pwd", "secret", "token", "api_key", "密码", "密钥",
];

/// 检测 `s` 开头是否为 `密钥名 = 值`；返回 (键与等号长度, 整个匹配长度)
fn secret_value_range(s: &str) -> Option<(usize, usize)> {
    let lower = s.to_ascii_lowercase();
    for key in SECRET_KEYS {
        if let Some(rest) = lower.strip_prefix(key) {
            let key_len = key.len();
            let after = rest.chars().next()?;
            if after != '=' && after != ':' && after != '：' {
                continue;
            }
            let eq_len = after.len_utf8();
            let value_start = key_len + eq_len;
            let value_len = s[value_start..]
                .find([' ', ',', ';', '\n', '\t'])
                .unwrap_or(s[value_start..].len());
            if value_len == 0 {
                continue;
            }
            return Some((key_len + eq_len, key_len + eq_len + value_len));
        }
    }
    None
}

/// 手机号长度检测：`1[3-9]\d{9}`
fn mobile_len(s: &str) -> Option<usize> {
    let b = s.as_bytes();
    if b.len() < 11 || b[0] != b'1' || !(b'3'..=b'9').contains(&b[1]) {
        return None;
    }
    b[2..11].iter().all(|&c| c.is_ascii_digit()).then_some(11)
}

/// 身份证号长度检测：`\d{17}[0-9Xx]`
fn idcard_len(s: &str) -> Option<usize> {
    let b = s.as_bytes();
    if b.len() < 18 {
        return None;
    }
    if !b[..17].iter().all(|&c| c.is_ascii_digit()) {
        return None;
    }
    let last = b[17];
    (last.is_ascii_digit() || last == b'X' || last == b'x').then_some(18)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 每日文件写入与命名() {
        let dir = tempfile::tempdir().unwrap();
        let mut w = DailyFileWriter::new(dir.path().to_path_buf());
        writeln!(w, "测试日志内容").unwrap();
        w.flush().unwrap();
        let today = Local::now().date_naive().format("%Y%m%d");
        let path = dir.path().join(format!("photos-{today}.log"));
        assert!(path.exists(), "日志文件应按日期命名");
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("测试日志内容"));
    }

    #[test]
    fn 清理过期日志仅保留近期() {
        let dir = tempfile::tempdir().unwrap();
        let today = Local::now().date_naive();
        let old = today - chrono::Duration::days(40);
        let recent = today - chrono::Duration::days(5);
        for d in [old, recent] {
            let p = dir
                .path()
                .join(format!("photos-{}.log", d.format("%Y%m%d")));
            std::fs::write(&p, "x").unwrap();
        }
        std::fs::write(dir.path().join("无关文件.txt"), "x").unwrap();
        let removed = cleanup_old_logs(dir.path(), 32).unwrap();
        assert_eq!(removed.len(), 1);
        assert!(
            !dir.path()
                .join(format!("photos-{}.log", old.format("%Y%m%d")))
                .exists()
        );
        assert!(
            dir.path()
                .join(format!("photos-{}.log", recent.format("%Y%m%d")))
                .exists()
        );
        assert!(dir.path().join("无关文件.txt").exists());
    }

    #[test]
    fn 脱敏手机号() {
        let out = redact("联系电话 13812345678 已登记");
        assert_eq!(out, "联系电话 138******** 已登记");
        // 不以 1[3-9] 开头的不动
        assert_eq!(redact("号码 10012345678"), "号码 10012345678");
    }

    #[test]
    fn 脱敏身份证() {
        let out = redact("身份证号 11010119900101123X");
        assert_eq!(out, "身份证号 110101********123X");
        let out2 = redact("ID 110101199001011231 结束");
        assert_eq!(out2, "ID 110101********1231 结束");
    }

    #[test]
    fn 脱敏密钥字段() {
        assert_eq!(redact("password=abc123"), "password=***");
        assert_eq!(redact("密码：机密内容，勿泄露"), "密码：***");
        assert_eq!(redact("token=abc,next=1"), "token=***,next=1");
    }

    #[test]
    fn 普通文本不受影响() {
        let s = "处理完成，输出到 data/out/task_1_white.jpg";
        assert_eq!(redact(s), s);
    }

    /// 测试用内存写入器（收集日志文本）
    #[derive(Clone, Default)]
    struct TestBuf(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

    struct TestSink(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

    impl std::io::Write for TestSink {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'a> MakeWriter<'a> for TestBuf {
        type Writer = TestSink;

        fn make_writer(&'a self) -> Self::Writer {
            TestSink(self.0.clone())
        }
    }

    #[test]
    fn 结构化字段按类型输出并脱敏() {
        let buf = TestBuf::default();
        let subscriber = tracing_subscriber::fmt()
            .event_format(ChineseLogFormat)
            .with_writer(buf.clone())
            .with_max_level(tracing::Level::INFO)
            .finish();
        tracing::subscriber::with_default(subscriber, || {
            let mut metrics = TaskMetrics::new();
            metrics.add("读图", 1.25);
            log_metrics(7, &metrics);
            log_error(
                "INVALID_PARAMS",
                "人脸检测",
                "处理失败：手机号 13812345678 无效",
                Some(7),
            );
        });
        let text = String::from_utf8(buf.0.lock().unwrap().clone()).unwrap();
        // 字符串字段不加引号、数值原样输出
        assert!(
            text.contains("任务分阶段耗时统计 任务=7 总耗时毫秒=1 分阶段耗时=读图 1.2ms"),
            "实际：{text}"
        );
        assert!(
            text.contains("错误码=INVALID_PARAMS 阶段=人脸检测"),
            "实际：{text}"
        );
        // 结构化字段值同样脱敏
        assert!(text.contains("138********"), "实际：{text}");
        assert!(!text.contains("13812345678"), "日志不得出现完整手机号");
    }
}
