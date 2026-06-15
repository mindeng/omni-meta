//! 顶层致命错误。格式内的局部损坏走 Warning，不进 Error。

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// 连容器格式都无法识别。
    UnrecognizedFormat,
    /// I/O 源直接报错。v1 不保留底层 io::Error 细节（best-effort）。
    Io,
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Error::UnrecognizedFormat => f.write_str("unrecognized file format"),
            Error::Io => f.write_str("i/o error"),
        }
    }
}

#[cfg(feature = "std")]
impl std::error::Error for Error {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_renders() {
        assert_eq!(
            alloc::format!("{}", Error::UnrecognizedFormat),
            "unrecognized file format"
        );
    }

    #[test]
    fn io_display_renders() {
        assert_eq!(alloc::format!("{}", Error::Io), "i/o error");
    }
}
