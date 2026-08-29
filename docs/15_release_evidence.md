# 15. v1 リリース検証記録

本書はtag作成者が、`docs/12_security.md` の公開前検証と公開後照合を記録するための様式を
定める。記入済み記録は運用者のprivateな保管場所へ置き、repositoryへcommitしない。
リポジトリをCI/CD目的で一時的にpublicへする場合も同じ扱いとする。

検証は、(A) tag作成前のcandidate確認、(B) tag pushで起動した自動release、(C) 公開後照合、の
3段階に分ける。tagのpush自体を公開指示として扱うため、Aを完了する前にtagを作成してはならない。

## 記録の機密性と追跡性

password、setup/device token、Cookie、署名鍵・secret値、Vaultのasset id・filename・path、
実画像、検索語、request/response body、機微headerは記録しない。publicなIssue、PR、Actions log、
GitHub Releaseにはpublic/private IP、内部hostname、Pangolin/Newtの管理URLも記録しない。

privateな証跡には、どの設置環境を検証したか追跡できる非機密のdeployment target IDと、
proxy/firewall設定のrevisionまたはhashを残す。生のendpointが監査上必要なら、厳格にアクセス制限
した別添に保存し、そのrecord IDだけを参照する。Releaseへ公開してよいprovenanceは、artifact名、
artifact SHA-256、署名証明書SHA-256 fingerprint、container image名/digest、tag、commitに限定する。

## A. tag作成前のcandidate確認

### 自動検証と署名済みartifact

- [ ] 対象version (`vX.Y.Z`) とcandidate commit SHAを確定した
- [ ] `uv run --no-project scripts/check-versions.py --tag vX.Y.Z` が成功した
- [ ] candidate commitの `ci-ok` とCodeQL 3言語が成功した
- [ ] Repository secret scan、Cargo/npm/Python audit、server/ML Trivy scanが成功した
- [ ] workflow_dispatch dry-runでfull CI、build、scan、Android署名・検証が成功した
- [ ] dry-runがGHCR tag、GitHub Release、repository contentsへ書き込んでいない
- [ ] Android署名secret 3点を `release-signing` environmentだけに登録した
- [ ] unsigned build jobに署名secretが渡っていない
- [ ] production artifactはGitHub Actionsだけで生成し、ローカル成果物を混入させない
- [ ] server / MLのGHCR packageがprivateである

例外を許せるのは、`docs/12_security.md` で明示した期限付き・image別Trivy allowlistと、
`.cargo/audit.toml` の非脆弱性RustSec勧告だけである。実際の脆弱性は例外にしない。

```text
version:
candidate commit SHA:
CI run URL:
CodeQL run URL:
release dry-run URL:
signed artifact ID/name:
candidate APK SHA-256:
signing certificate SHA-256 fingerprint:
permitted exception record IDs: none / private record references
```

### Pangolin / Newt とorigin遮断

candidate commitを実際の回線・proxy・firewallを使う検証環境へ配備して行う。詳細なendpointは
publicな場所へ記録しない。

- [ ] 外部回線からPangolin認証なしでIllumiaの保護対象APIへ到達できない
- [ ] Pangolin認証後もIllumia自身の認証なしでは保護対象情報を取得できない
- [ ] 改ざんtoken、異なるOrigin、oversize body、image bomb、SQLi metacharacter、
      path traversal、WS connection floodを拒否する
- [ ] Vault lock中の存在秘匿対象が404となり、asset id・filenameを漏らさない
- [ ] IPv4のorigin IP / host portへ直接到達できない
- [ ] IPv6のorigin address / host portへ直接到達できない
- [ ] router port-forward、UPnP、IPv6 firewallに迂回経路がない
- [ ] public resourceの国別ruleが `Pass to Auth` であり、`Allow` ではない
- [ ] CrowdSecの導入、bouncer、access log、更新を確認した
- [ ] 外部はHTTPSのみで、HSTS、証明書更新、Pangolin管理画面MFAを確認した
- [ ] Pangolin管理APIが外部へ公開されていない

```text
UTC日時:
確認者:
deployment target ID:
proxy/firewall config revision or hash:
外部回線区分: mobile / separate ISP / other
Pangolin/Newt adversarial suite: pass / fail
IPv4 origin isolation: pass / fail
IPv6 origin isolation: pass / fail
```

### Reverse proxy とログ

- [ ] proxyとIllumia双方のupload/body size上限を確認した
- [ ] request、body、idle、WebSocket timeoutとWebSocket同時接続数上限を確認した
- [ ] login/setupのrate limitと同時Argon2上限を確認した
- [ ] trusted proxy CIDRとclient IP attributionを確認した
- [ ] access logからquery、Authorization、Cookie、機微headerを除外した
- [ ] Vault requestがasset id・filenameを含まない正規化pathで記録される
- [ ] log retentionと閲覧権限を確認した

### Android実機

- [ ] dry-runの署名済み `android-apk` artifactを実機へ導入し、起動・login・主要閲覧・
      再起動後の再loginを確認した
- [ ] `apksigner verify --print-certs` が成功し、記録したfingerprintと一致した
- [ ] 以前の正式版がある場合、同じ署名鍵で上書き更新できた

```text
device class / Android major version:
install/update smoke test: pass / fail
```

## B. tag pushによる自動release

Aを完了した後だけtagをpushする。workflowはfull CI、build、scan、signを再実行し、scan済みの
container candidate digestだけをversion/`latest`へpromotionしてGitHub Releaseを作る。

- [ ] tagのversionとtarget commitがAのversion / candidate commitに一致する
- [ ] tag release runのfull CI、全build/scan、Android署名・検証が成功した
- [ ] Trivyが成功したcandidate digestとpromotion対象digestが同一である
- [ ] 単一signer、APK SHA-256、署名証明書SHA-256のfail-closed検証が成功した
- [ ] Release添付assetが署名済みAPKと検証済みprovenance recordのallowlistだけである
- [ ] provenance recordはartifact名・SHA-256・署名証明書SHA-256・image名/digest・tag・commit
      だけを含み、Release notes / change logに「記録の機密性と追跡性」の禁止情報がない
- [ ] unsigned desktop artifact、unsigned APK、Docker build recordがReleaseへ添付されていない

```text
tag release run URL:
server candidate digest: sha256:<64 hex>
ML candidate digest: sha256:<64 hex>
released APK SHA-256:
released signing certificate SHA-256 fingerprint:
```

## C. 公開後の照合

- [ ] Releaseが指すtag / target commitがA/Bのversionとcandidate commitに一致する
- [ ] server / MLのversion tagと`latest`が、いずれもcandidate digestに一致する
- [ ] ReleaseからAPKをdownloadし、SHA-256を再計算してBのrecordとRelease notesに一致する
- [ ] downloadしたAPKへ `apksigner verify --print-certs` を実行し、証明書SHA-256を再取得して
      BのrecordとRelease notesに一致する
- [ ] 公開assetがallowlistどおりで、provenanceに許可項目以外がなく、Release notes / change logに
      「記録の機密性と追跡性」の禁止情報がない
- [ ] 許可したアカウントの `read:packages` tokenでdigest固定pullが成功する
- [ ] 権限のない未認証clientからprivate GHCR packageをpullできない
- [ ] production ComposeがRelease notesの64桁digestを使い、`latest`へfallbackしない
- [ ] CI/CDのためrepositoryを一時publicにした場合、作業後にprivateへ戻した

GitHub Releaseをrepositoryの一時public期間中に公開することは許可する。GitHub Releaseがpublicでも、
server / MLのGHCR packageはprivateを維持する。CI/CD作業後はrepositoryをprivateへ戻し、恒久public
運用へ変更する場合は別途docs変更で運用方針を明示する。

照合不一致や意図しない情報公開があれば新規配布を直ちに止め、影響するRelease・package tag・
credentialをincident responseの対象にする。同じversion tagを別digestへ付け替えず、修正後は
新しいversionでreleaseする。

```text
post-release verification UTC:
tag release run URL:
release URL:
authenticated digest pull: pass / fail
unauthenticated pull rejection: pass / fail
repository visibility restored: yes / not applicable (never made public)
result: pass / distribution stopped
```
