# 15. v1 リリース検証記録

本書は `release-production` environment の承認者が、`docs/12_security.md` の公開前ゲートを
確認するための記録様式を定める。記入済み記録は運用者のprivateな保管場所へ置き、repositoryへ
commitしない。リポジトリをCI/CD目的で一時的にpublicへする場合も同じ扱いとする。

## 記録してはいけない情報

- password、setup/device token、Cookie、署名鍵、secret値
- public IP、private IP、内部hostname、Pangolin/Newtの管理URL
- Vaultのasset id、filename、path、stack/cluster名
- 実画像、検索語、request/response body、機微header

記録には成功/失敗、UTC日時、候補commit、GitHub Actions run URL、署名fingerprint、
匿名化した端末/ネットワーク区分だけを残す。失敗時の生ログはアクセス制限された場所へ保管し、
IssueやPRへ貼らない。

## 1. 候補の同一性

- [ ] 対象version (`vX.Y.Z`) と候補commit SHAを記録した
- [ ] `uv run --no-project scripts/check-versions.py --tag vX.Y.Z` が成功した
- [ ] 候補commitの `ci-ok` とCodeQL 3言語が成功した
- [ ] Repository secret scan、Cargo/npm/Python audit、server/ML Trivy scanが成功した
- [ ] production artifactはGitHub Actionsだけで生成され、ローカル成果物を混入していない
- [ ] imageはscan済みcandidate digestとpromotion対象digestが同一である

記録欄:

```text
version:
commit SHA:
CI run URL:
CodeQL run URL:
release dry-run URL:
server candidate digest: sha256:<64 hex>
ML candidate digest: sha256:<64 hex>
```

## 2. Pangolin / Newt とorigin遮断

Illumiaを設置する実際の回線・proxy・firewallで行う。詳細なIPやdomainは記録しない。

- [ ] 外部回線からPangolin認証なしでIllumiaの保護対象APIへ到達できない
- [ ] Pangolin認証後もIllumia自身の認証なしでは保護対象情報を取得できない
- [ ] 改ざんtoken、異なるOrigin、oversize body、path traversal、WS floodを拒否する
- [ ] IPv4のorigin IP / host portへ直接到達できない
- [ ] IPv6のorigin address / host portへ直接到達できない
- [ ] router port-forward、UPnP、IPv6 firewallに迂回経路がない
- [ ] public resourceの国別ruleが `Pass to Auth` であり、`Allow` ではない
- [ ] Pangolin管理画面のMFA、証明書更新、HSTSを確認した

記録欄:

```text
UTC日時:
確認者:
外部回線区分: mobile / separate ISP / other
Pangolin/Newt adversarial suite: pass / fail
IPv4 origin isolation: pass / fail
IPv6 origin isolation: pass / fail
```

## 3. Reverse proxy とログ

- [ ] proxyとIllumia双方のupload/body size上限を確認した
- [ ] request、body、idle、WebSocket timeoutを確認した
- [ ] login/setupのrate limitと同時Argon2上限を確認した
- [ ] trusted proxy CIDRとclient IP attributionを確認した
- [ ] access logからquery、Authorization、Cookie、機微headerを除外した
- [ ] Vault requestがasset id・filenameを含まない正規化pathで記録される
- [ ] log retentionと閲覧権限を確認した

## 4. Android署名と実機

- [ ] `release-signing` environmentにrequired reviewerを設定した
- [ ] 署名secret 3点をenvironment secretだけに登録した
- [ ] unsigned build jobに署名secretが渡っていない
- [ ] `apksigner verify --print-certs` が成功した
- [ ] Release記載のAPK SHA-256と署名fingerprintが配布APKに一致した
- [ ] 配布APKを実機へ導入し、起動・login・主要閲覧・再起動後の再loginを確認した
- [ ] 以前の正式版がある場合、同じ署名鍵で上書き更新できた

記録欄:

```text
device class / Android major version:
APK SHA-256:
signing certificate SHA-256 fingerprint:
install/update smoke test: pass / fail
```

## 5. GitHub repository設定

- [ ] `main` rulesetがPR経由を必須にしている
- [ ] `ci-ok` とCodeQLのrequired checkを設定した
- [ ] force-pushとbranch deletionを禁止した
- [ ] Dependency Graph、Dependabot/vulnerability alertsを有効化した
- [ ] code scanning、secret scanning、private vulnerability reportingを有効化した
- [ ] Security画面の未解決alertが0件、または期限・owner・根拠付き例外だけである
- [ ] `release-production` environmentにrequired reviewerを設定し、secretを置いていない
- [ ] `release-signing` environment以外にAndroid署名secretが存在しない

設定画面のscreenshotにはrepository secret名以外の機密情報を含めず、privateな記録場所へ保存する。

## 6. Private配布

- [ ] server / MLのGHCR packageがprivateである
- [ ] 許可したアカウントの `read:packages` tokenでdigest固定pullが成功した
- [ ] 権限のない未認証clientからprivate packageをpullできない
- [ ] production Composeがrelease notesの64桁digestを使い、`latest`へfallbackしない
- [ ] CI/CD後に通常運用へ戻す場合、repository visibilityをprivateへ戻した

GitHub Releaseをrepositoryの一時public期間中に公開することは許可する。その場合は第三者から
閲覧・download可能な内容だけであることを承認者が確認する。

## 7. 承認と中止条件

全項目が成功し、候補commitとCI/artifact/digestの同一性を確認した担当者だけが
`release-production` environmentを承認する。次のいずれかに該当する場合は承認せず、tag runを
cancelして修正PRを作る。

- 未実施、結果不明、候補commit不一致、期限切れ例外
- security alert、secret検出、署名fingerprint不一致
- origin迂回、proxy limit/log不備、実機smoke失敗
- scan後のrebuild、candidateとpromotion digestの不一致

承認記録:

```text
approved UTC:
reviewer:
candidate commit SHA:
external evidence record ID/location:
exceptions: none / private record reference
```
