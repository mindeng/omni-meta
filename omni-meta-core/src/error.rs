//! 顶层致命错误。格式内的局部损坏走 Warning，不进 Error。

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// 连容器格式都无法识别。
    UnrecognizedFormat,
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Error::UnrecognizedFormat => f.write_str("unrecognized file format"),
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
}
