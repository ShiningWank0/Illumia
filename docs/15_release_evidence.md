# 15. v1 リリース検証記録

本書は `release-production` environment の承認者が、`docs/12_security.md` の公開前ゲートを
確認するための記録様式を定める。記入済み記録は運用者のprivateな保管場所へ置き、repositoryへ
commitしない。リポジトリをCI/CD目的で一時的にpublicへする場合も同じ扱いとする。

検証は、(A) tag作成前、(B) tag runのcandidate生成後かつproduction承認前、(C) 公開後照合、の
3段階に分ける。承認後にしか存在しないRelease資産やpromotion済みtagを、承認前の証跡として
扱ってはならない。

## 記録の機密性と追跡性

password、setup/device token、Cookie、署名鍵・secret値、Vaultのasset id・filename・path、
実画像、検索語、request/response body、機微headerは記録しない。publicなIssue、PR、Actions log、
GitHub Releaseにはpublic/private IP、内部hostname、Pangolin/Newtの管理URLも記録しない。

privateな証跡には、どの設置環境を検証したか追跡できる非機密のdeployment target IDと、
proxy/firewall設定のrevisionまたはhashを残す。生のendpointが監査上必要なら、厳格にアクセス制限
した別添に保存し、そのrecord IDだけを参照する。Releaseへ公開してよいprovenanceは、artifact名、
artifact SHA-256、署名証明書SHA-256 fingerprint、container image名/digest、tag、commitに限定する。

## A. tag作成前の準備

- [ ] 対象version (`vX.Y.Z`) と候補commit SHAを確定した
- [ ] `uv run --no-project scripts/check-versions.py --tag vX.Y.Z` が成功した
- [ ] 候補commitの `ci-ok` とCodeQL 3言語が成功した
- [ ] Repository secret scan、Cargo/npm/Python audit、server/ML Trivy scanが成功した
- [ ] CodeQLを含むGitHub Security画面の未解決security alertが0件である
- [ ] main rulesetがPR、`ci-ok`、CodeQL、Dependency Reviewを必須にしている
- [ ] mainへのforce-pushとbranch deletionを禁止した
- [ ] Dependency Graph、Dependabot/vulnerability alerts、code/secret scanning、
      private vulnerability reportingを有効化した
- [ ] `release-signing` environmentにrequired reviewerを設定し、Android署名secret 3点を
      このenvironmentだけに登録した
- [ ] `release-production` environmentにrequired reviewerを設定し、secretを登録していない
- [ ] workflow_dispatchのdry-runでfull CI、build、scan、unsigned desktop除外が成功した
- [ ] production artifactはGitHub Actionsだけで生成し、ローカル成果物を混入させない
- [ ] server / MLのGHCR packageがprivateである

例外を許せるのは、`docs/12_security.md` で明示した期限付き・image別Trivy allowlistと、
`.cargo/audit.toml` の非脆弱性RustSec勧告だけである。一般のsecurity alertや実際の脆弱性は
例外にしない。

記録欄:

```text
version:
candidate commit SHA:
CI run URL:
CodeQL run URL:
release dry-run URL:
deployment target ID:
proxy/firewall config revision or hash:
permitted exception record IDs: none / private record references
```

## B. candidate検証とproduction承認

tag push後、workflowはscan済みcontainerをtagなしのcandidate digestとしてprivate GHCRへpushし、
署名済みAPKをActions artifactとして生成する。`release-production` の承認待ちで停止している間に、
次を確認する。GitHub Releaseとversion/`latest` image tagはこの時点ではまだ存在しない。

### candidateの同一性

- [ ] tag release runのcommitがAで承認したcommitと一致する
- [ ] full CI、全build/scan、`release-signing`、APK署名検証が成功した
- [ ] candidate digestを記録し、そのdigestを対象にTrivyが成功した
- [ ] `android-apk` artifactのAPK SHA-256と署名証明書SHA-256 fingerprintを記録した
- [ ] `apksigner verify --print-certs` が成功した
- [ ] unsigned build jobに署名secretが渡らず、unsigned APKが配布対象にない

```text
tag release run URL:
server candidate digest: sha256:<64 hex>
ML candidate digest: sha256:<64 hex>
signed artifact ID/name:
candidate APK SHA-256:
signing certificate SHA-256 fingerprint:
```

### Pangolin / Newt とorigin遮断

candidate digestを実際の回線・proxy・firewallへ配備して行う。詳細なendpointはpublicな場所へ
記録しない。

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

- [ ] `android-apk` Actions artifactを実機へ導入し、起動・login・主要閲覧・
      再起動後の再loginを確認した
- [ ] 以前の正式版がある場合、同じ署名鍵で上書き更新できた

```text
device class / Android major version:
install/update smoke test: pass / fail
```

### 承認

AとBの全項目が成功し、candidate commit、CI、artifact、digestの同一性を確認した担当者だけが
`release-production` environmentを承認する。未実施、結果不明、候補不一致、security alert、
secret検出、期限切れ例外、origin迂回、proxy/log不備、実機smoke失敗があれば承認せずrunを
cancelして修正PRを作る。

```text
approved UTC:
reviewer:
candidate commit SHA:
external evidence record ID/location:
permitted exceptions: none / private record references
```

## C. 公開後の照合

承認後、workflowはscan済みcandidate digestにversion/`latest` tagを付け、GitHub Releaseを作る。
完了したrunに対して次を照合する。

- [ ] server / MLのversion tagとpromotion対象digestがcandidate digestに一致する
- [ ] ReleaseのAPK SHA-256と署名証明書SHA-256 fingerprintがcandidate artifactに一致する
- [ ] Release notes・artifact・provenanceが、上記の公開可能情報だけを含む
- [ ] 許可したアカウントの `read:packages` tokenでdigest固定pullが成功する
- [ ] 権限のない未認証clientからprivate GHCR packageをpullできない
- [ ] production ComposeがRelease notesの64桁digestを使い、`latest`へfallbackしない
- [ ] CI/CD後に通常運用へ戻す場合、repository visibilityをprivateへ戻した

GitHub Releaseをrepositoryの一時public期間中に公開することは許可する。その場合は第三者から
閲覧・download可能な内容だけであることを公開前に確認する。GitHub Releaseがpublicでも、
server / MLのGHCR packageはprivateを維持する。

照合不一致や意図しない情報公開があれば新規配布を直ちに止め、影響するRelease・package tag・
credentialをincident responseの対象にする。同じversion tagを別digestへ付け替えず、修正後は
新しいversionでreleaseする。

```text
post-release verification UTC:
tag release run URL:
release URL:
authenticated digest pull: pass / fail
unauthenticated pull rejection: pass / fail
repository visibility restored: yes / no / intentionally public
result: pass / distribution stopped
```
