# 12. セキュリティ設計・公開運用

本書は Illumia を悪意ある第三者から継続的に攻撃される前提での必須要件を定める。
Pangolin / Newt、国別ルール、CrowdSec 等の edge 防御は重要だが、Illumia 自身の認証・
入力検証を置き換えるものではない。

## 脅威モデル

防御対象:

- 原本画像、metadata、検索語、stack 名、device token、Vault の存在・内容・鍵素材。
- NAS の CPU・メモリ・ディスク・DB connection・worker/thread。
- 初回セットアップ権、設定変更権、削除・Vault import/export を含む操作権。
- server / reverse proxy / browser Console / URL history / crash report / CI log。

想定する攻撃:

- 未認証および認証済み攻撃者による総当たり、巨大 body、画像 decompression bomb、
  connection 枯渇、検索や設定値を用いた資源枯渇。
- SQL injection、path traversal、XSS、CSRF、clickjacking、token replay、URL・ログからの
  credential 窃取、初回セットアップの横取り。
- 改ざん画像・暗号 blob・DB/keyfile・依存 package・container image を利用した攻撃。
- VPN / proxy / botnet により日本国内 IP と判定される攻撃。国別判定は認証要素に数えない。

ブラウザ拡張、既に管理者権限を奪われた NAS host、ロック解除中の端末を物理的に操作できる
攻撃者を完全には防げない。ただし侵害時の露出を抑えるため、秘密を Web Storage・URL・
通常ログへ残さず、container と filesystem の権限を最小化する。

## 認証境界

Illumia はシングルユーザーで、Illumia 自身にはログイン ID がない。認証要素は共有
パスワードであり、`device_name` は token 一覧の表示ラベルにすぎない。ログイン成功後は
失効可能な 256-bit device token を使う。Pangolin のユーザー ID / password 保護を有効に
した場合は、その外側に独立した第二の認証境界が加わる。

### 初回セットアップ

- loopback 以外で未セットアップの server を起動する場合、十分にランダムな
  `ILLUMIA_SETUP_TOKEN` を必須とする。値は 32 文字以上、256 byte 以下とし、server は
  SHA-256 のみを memory に保持する。
- `/api/auth/setup` は `X-Illumia-Setup-Token` を定数時間比較し、一度セットアップが
  完了した後は永久に 409 とする。
- setup token は URL、compose file、Git、画像、ログへ書かない。secret 管理機能または
  permission 0600 の `.env` で渡し、完了後に rotate / 削除する。
- reverse proxy 公開より先に初期セットアップを完了することを推奨する。

### Web とネイティブ

- Web は同一オリジン Cookie 認証とする。Cookie は `HttpOnly`, `SameSite=Strict`,
  `Path=/api`、HTTPS 公開時は `Secure`。Web の setup / login response body に device
  token を返さず、JavaScript から保存・参照しない。
- Cookie 認証の非 safe method は同一 authority の `Origin` を必須とする。
- ネイティブは Bearer token を使い、永続化する場合は OS secure storage に限る。M5 Androidは
  Keystore未実装のため永続化せずRust process memoryのみに保持し、login/setup responseから
  tokenをWebViewへ返さず、Rust bridgeだけがAuthorizationを付与する。再起動後は再ログインする。
  Vault passwordをJavaScript Mapへ保存する生体認証代替は認証なしで読めるため禁止する。
- ネイティブの接続先 URL は保存・読み出し・接続の各時点で検証する。`external` は
  `https` のみ。credential 埋め込み・query・fragment・path・制御文字を含む URL は拒否する
  (URL パーサは tab/改行を黙って除去するため、parse 前の生文字列で弾く)。
- 接続先の選択順は **external → local**。平文 HTTP の `local` は自動選択せず、
  使用のたびに利用者の明示確認を取る。平文HTTPのhostはprivate/loopback/link-localの
  IP literalまたはRFC `localhost`名だけを許可する。一般の`.local`/DNS hostnameはprobe後の
  name rebindingで別IPへBearerを送れるため拒否し、hostnameにはHTTPSを必須とする。
- クライアントは初回接続で server の `instance_id` を pin し、pin と一致しない server へは
  credential を送らない。到達性プローブは 2xx のみでは信用せず、response schema と
  `instance_id` を検証する。これは信頼できない Wi-Fi 上で攻撃者が同じ private IP に
  偽 server を立てる攻撃に対する防御であり、初回登録時のみ TOFU になる。
- Android の native HTTP bridge は、identity 検証済み origin へプロセス中 1 回だけ bind し、
  WebView からの自動再 bind を禁止する。request URL は join/正規化後の origin と `/api/`
  境界を再検証し、encoded dot / slash / backslash を path に含む入力を拒否する。
- native response は `Content-Length` を先に検査した上で chunk 読み込みへ固定上限を適用し、
  未知長 response も上限超過時点で中止する。network call には有限の deadline、画像 decode
  には dimension と allocation 上限を設ける。派生画像のnative受信上限は thumbnail 2 MiB、
  preview 16 MiB とし、汎用API responseは4 MiBとする。
- Android の原本は WebView IPC で Base64 全量化せず、native save dialog が返した保存先へ
  直接 stream する。汎用 bridge から original endpoint を呼ぶことは禁止し、native download
  command は正規化済みの main/Vault original path、UUID、headers、総byte数を再検証する。
- Android汎用bridgeのrequestはBase64 IPC増幅を考慮し、multipart overhead込み17 MiBを
  TypeScriptのArrayBuffer直後とRustのBase64 decode前/後で検査する。大容量uploadはnative
  content-URI streaming commandが完成するまで拒否し、通常Web/serverの128 MiB上限と分離する。
- token は URL query、WebSocket URL、HTML、通常ログ、error message に含めない。
- WS の Cookie 認証でも同一 authority の `Origin` を要求する。
- login/setup には失敗回数と同時 Argon2 実行数の上限を設ける。edge 側にも送信元 IP
  単位の rate limit を設ける。
- server が proxy header を使うのは immediate peer が `ILLUMIA_TRUSTED_PROXY_CIDRS` に
  含まれる場合だけとする。`X-Forwarded-For` は右端から trusted hop を除いて送信元を求め、
  incoming headerへappendするproxy構成でも利用者指定の左端値をrate-limit keyにしない。
  送信元bucketのmemory上限時は新規送信元を共有overflow bucketへ集約しない。最終失敗が最も古い
  bucketをevictし、攻撃者がbucket上限を埋めても別の新規利用者へ失敗回数を継承させない。

## Pangolin / Newt で公開するときの必須チェック

1. public resource は Pangolin authentication を有効にする。Illumia のパスワードとは
   別の強い password と MFA (利用可能な場合) を使う。
2. 日本限定ルールは `country=JP` を **Pass to Auth**、最後を `Deny` とする。
   `Allow` は Pangolin authentication を通さず許可する動作なので使用しない。
3. GeoIP は誤判定があり、VPN / proxy / 国内 bot で回避できる。補助的な削減策としてのみ扱う。
4. CrowdSec は別途導入・bouncer 設定・access log 設定・更新を確認する。Pangolin/Newt を
   動かしただけで怪しい通信が自動的に全て block されるとは仮定しない。
5. 外部は HTTPS のみ。HSTS、証明書更新、Pangolin 管理画面の MFA、管理 API の非公開を確認する。
6. Illumia の host port は既定で `127.0.0.1` のみに bind する。Newt が別 container の場合は
   明示した private Docker network または host gateway を使う。router の port forward、
   UPnP、IPv6 firewall を含め、Pangolin を迂回する経路を閉じる。
7. proxy と app の upload/body timeout・size limit を両方設定する。WebSocket の idle timeout
   と同時接続数も制限する。
8. 公開後に外部回線から origin IP/host port へ直接到達できないこと、未認証で
   `/api/server/info` 以外の情報が得られないことを確認する。

## HTTP・browser 防御

- CORS は同一オリジンを既定とし、`*` を許可しない。ネイティブ HTTP client に CORS は不要。
- `Content-Security-Policy` (`default-src 'self'`, `object-src 'none'`,
  `frame-ancestors 'none'`, `base-uri 'none'` 等)、`X-Content-Type-Options: nosniff`,
  `Referrer-Policy: no-referrer`, `Permissions-Policy` を全 response に付ける。
- API response は既定で `Cache-Control: private, no-store`。認証済みの派生画像だけ
  private browser cache を許可し、共有 proxy cache には保存させない。
- browser/WebView の認証済み Object URL cache は件数とBlob合計byte数の両方を制限する。
  eviction・Vault lock・logoutではURL revokeとbyte会計を不可分に行い、Vault lockと競合した
  in-flight responseを後からcacheへ追加しない。thumbnail/previewはbrowserでもresponse streamを
  endpoint別上限までだけ読み、上限判定前に`Response.blob()`で全量bufferしない。
- API body・配列・文字列・画像 dimension/decode allocation・検索結果・WS connection/frame に
  固定上限を設ける。上限超過は処理・allocation 前に 400/413/429 で拒否する。
- 画像原本のfile read、decoder、RGBA変換、resize、encode、ThumbHashの同時実行数は job
  worker 数とは独立した process-wide 上限で制御する。permit待ちworkerが原本 `Vec` を
  先に保持してはならない。
  Vault の暗号化 blob は固定長 channel でチャンク復号し、同時配信数を制限することで、
  大容量画像や slow client を組み合わせても平文全体や無制限の先読みを memory に保持しない。
- HTTP connection は絶対寿命と bounded graceful drain を持つ。response body の frame timeout が
  socket backpressure 中に poll されない場合でも slow client が connection/stream permit を無期限に
  保持できないことを adversarial test する。Vault channel の連続fullにも短いdeadlineを設け、打切りは
  正常EOFへ偽装せずbody errorで通知する。
- 同期 SQLite mutex を取得する大規模 stack/cluster/list/search は bounded `spawn_blocking` admission
  へ隔離し、Tokio worker を mutex wait や大量 row 反復で占有しない。
- main ingest/export の大容量 file write・復号・`sync_all` は SQLite mutex/transaction の外で
  完了させ、短い metadata commit だけを lock 内で行う。commit 失敗時は UUID 固有の新規 file を
  rollback する。
- user value は SQL bind parameter で渡す。LIKE wildcard は escape し、動的識別子は
  allowlist からのみ選ぶ。filesystem path は UUID と allowlist extension から生成し、
  DB 由来 relative path も component 単位で検証する。
- upload は拡張子だけで decoder を増やさず、必要 format の decoder だけを build する。
  format sniffing で別 format の parser へ迂回させない。

## ログ・Console・エラー

- request log は method と正規化した path のみ。query、Authorization/Cookie、
  request/response body、検索語を記録しない。
- Vault path は `/api/vault/*` に正規化し、asset id、filename、stack/cluster 名を記録しない
  (→ docs/06)。
- browser に production の `console.log` を残さない。server error は内部詳細、SQL、
  filesystem path、header を client に返さず、client も token や Vault 情報を
  telemetry/crash report へ送らない。
- Web 認証成功 response は token を body に含めない。なお、利用者自身または端末を
  操作できる者が DevTools を開けば、入力した password / setup token は送信 request として
  観測できる。これは Web 認証の性質上隠せないため、その権限を持つ攻撃者は脅威モデル外とし、
  共有端末を使わず、端末ロックとブラウザプロファイル分離で防ぐ。
- reverse proxy / CrowdSec access log でも query string と sensitive header を除外し、
  retention と閲覧権限を制限する。

## Container・filesystem・supply chain

- runtime は非 root、`cap_drop: ALL`, `no-new-privileges`, read-only root filesystem、
  writable volume の限定、PID/CPU/memory 上限を既定とする。
- server と ML sidecar は異なる UID を使い、共有するのは専用 group で保護した UDS directory
  だけとする。ML へ application data volume を mount せず、model-only volume だけを read-only
  mount する。
- data directory は Unix では mode `0700`、DB と keyfile は `0600` とし、専用 UID/GID
  のみ読み書き可能にする。権限を変更できない共有 filesystem は公開運用に使わない。
  backup も同等に暗号化し、Vault keyfile と DB/blob を一緒に外部公開しない。
- build context は `.env*`, key, DB, image library、Git metadata を除外する。
- Cargo/npm/Python/Docker/GitHub Actions の dependency update と脆弱性監査を CI で行う。
  release job の token permission は job 単位の最小権限とし、SBOM/provenance を生成する。
- JavaScript/TypeScript、Python、Rust は CodeQL の `security-extended` query を
  main / PR / 週次で実行する。Rust はこれに加えて clippy、RustSec、adversarial
  integration test も必須とし、静的解析だけでruntime境界の検証を代替しない。
- Android signing key/passwordはnpm/Gradleやrepository codeを実行するbuild jobへ渡さない。
  full CI済みunsigned artifactを別jobへ渡し、そのjobはrepositoryをcheckoutせず、署名と
  fingerprint検証だけを行う。unsigned APKをGitHub Releaseへ添付しない。
- Docker base imageとGitHub Actionはdigest / commit SHAへ固定し、Dependabotで追従する。
- production artifact は GitHub Actions の固定 workflow だけで作り、review 済み commit と
  image digest を deployment 時に記録する。container はtag無しcandidate digestとして1回だけbuildし、
  registry上の同じdigestをscanする。全container candidateのscanと他の配布build/sign jobの
  成功後に単一jobからのみversion/`latest` tagへpromoteする。scan前やmatrix途中にrelease tagを
  公開せず、scan後の再buildで配布物を差し替えない。
- production Compose は server/ML の repository と `@sha256:` を YAML 内で固定し、環境変数には
  64桁 digest 本体だけを要求する。未設定時に
  `latest` や local build へ fallback しない。production file に `build:` を置かず
  `pull_policy: always` とする。起動 wrapper は両環境変数を 64-hex digest 形式で検証してから
  `docker compose ... up --no-build` を実行する。local build は明示的な dev overlay にだけ置く。
  model bundle も bundle 外の trusted digest/signature を pin する。

## 公開前の検証ゲート

- `cargo fmt --all --check`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo test --workspace`
- `cargo audit`
- Web の lint / `svelte-check` / unit test / build / `npm audit`
- Python の `ruff check` / `ruff format --check` / test
- CodeQL code scanning に未解決の security alert がないこと
- `docker compose config`、Dockerfile/Compose lint、CI での multi-stage image build と scan。
  Trivy は未修正を含む `HIGH,CRITICAL` を失敗扱いにする。例外は vulnerability ID・根拠・
  owner・失効期限を明示した期限付き allowlist としてimage別にレビューし、suppressed findingも
  CI logへ表示する。期限切れと新規IDは失敗扱いにし、修正版が出たIDは定期reviewでallowlistから
  削除する
- 認証なし、改ざん token、異なる Origin、oversize body、画像 bomb、SQLi metacharacter、
  path traversal、WS connection flood、Vault lock 中の 404 秘匿を含む adversarial test

## 公開前に必須の実環境検証 (v0.2.0 時点で未実施)

以下は Illumia のコードではなく**設置環境**に対する検証であり、実際の
Pangolin/Newt・回線・実機が無いと確認できない。**v0.2.0 はこれらを未実施のまま
リリースしている**。インターネットへ公開する前に必ず実施すること。
完了をもって v1.0 以降の公開運用へ進む。

- [ ] Pangolin/Newt 配下の外部回線から adversarial test を実施する
- [ ] 外部回線から origin IP / host port へ直接到達できないことを IPv4 / IPv6 双方で確認する
      (router の port forward、UPnP、IPv6 firewall を含め Pangolin を迂回する経路を閉じる)
- [ ] 配布 APK を実機へ導入し、動作と署名証明書の fingerprint を確認する
- [ ] reverse proxy の upload/body/idle timeout、rate limit、sensitive header のログ除外を確認する
- [ ] GitHub の Dependabot / code scanning / secret scanning alerts を確認する
- [ ] `main` ruleset で `ci-ok` / CodeQL を必須化し、force-push / deletion を禁止する
- [ ] `release-signing` environment に required reviewer とAndroid署名secret 3点を設定し、
      workflow_dispatchのdry-run成功後、tag releaseで署名fingerprintを照合する
- [ ] CI枠確保のため一時的にpublicへ変更したrepositoryをprivateへ戻し、その期間に本番tag・
      Release・packageを公開していないことを確認する。server / ML のGHCR packageもprivateを
      維持し、許可したアカウントの `read:packages` tokenでrelease notes記載のimmutable digestを
      pullできることを確認する

コード側の防御 (認証境界・入力検証・資源上限・container 権限・supply chain gate) は
CI で継続的に検証している。上記は「その外側」の設置作業であり、CI では代替できない。
