// Illumia Android (Tauri 2)。web/ の SPA を WebView に包む薄いシェル。
// ネイティブ機能はプラグイン経由で web 側 (アプリモード) から呼ぶ:
//   - biometric: vault アンロックの代替 (→ docs/06 / docs/08)
//   - dialog / fs: 自動アップロードのフォルダ選択・走査
//   - http: リモートサーバーへの CORS 非対象な HTTP (Bearer 認証)

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_http::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_biometric::init())
        .run(tauri::generate_context!())
        .expect("error while running Illumia Android");
}
