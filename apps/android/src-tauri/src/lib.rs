// Illumia Android (Tauri 2)。web/ の SPA を WebView に包む薄いシェル。
// ネイティブ機能はプラグイン経由で web 側 (アプリモード) から呼ぶ:
//   - biometric: vault アンロックの代替 (→ docs/06 / docs/08)
//   - dialog / fs: 自動アップロードのフォルダ選択・走査
//
// HTTP は汎用 plugin-http を frontend へ公開せず、登録済み Illumia サーバー宛の
// 呼び出しだけを通す専用 command (bridge) に限定する (→ docs/12, SEC-004)。

mod bridge;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let builder = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_fs::init());

    // biometric はモバイル専用プラグイン。デスクトップ (dev ビルド・単体テスト) では
    // 登録しない。これがないと desktop ターゲットでコンパイルが通らない。
    #[cfg(mobile)]
    let builder = builder.plugin(tauri_plugin_biometric::init());

    builder
        .manage(bridge::ServerBinding::default())
        .manage(bridge::build_client())
        .invoke_handler(tauri::generate_handler![
            bridge::illumia_request,
            bridge::illumia_set_server
        ])
        .run(tauri::generate_context!())
        .expect("error while running Illumia Android");
}
