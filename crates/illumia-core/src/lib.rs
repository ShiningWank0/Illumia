//! illumia-core: ドメインロジック・DB・暗号・ジョブキュー (→ docs/01_architecture.md)。
//!
//! フレームワーク非依存。サービス層 trait 群をここで定義し、
//! illumia-server (HTTP) と illumia-desktop (in-process) の両方から使う。

/// アプリケーションのバージョン (workspace で一元管理)。
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_is_set() {
        assert!(!VERSION.is_empty());
    }
}
