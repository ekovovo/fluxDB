# 应用内更新

FluxDB 使用 GitHub Release 作为免费更新源。操作系统发布者证书是可选项，但更新
清单签名和下载包 SHA-256 校验不可关闭。

## 更新密钥

一次性生成 Ed25519 密钥：

```sh
bash scripts/generate-update-signing-key.sh
```

脚本不会提交私钥。将私钥内容保存为 GitHub Actions Secret
`FLUXDB_UPDATE_SIGNING_KEY`，将输出的 32 字节公钥 Base64 保存为 Actions Variable
`FLUXDB_UPDATE_PUBLIC_KEY`。发布构建在编译时嵌入公钥；未配置公钥的本地构建会明确
显示更新不可用。

私钥丢失后，现有安装无法验证后续更新，只能重新安装。请离线备份，并预留双公钥
轮换流程。

## Release 说明

每个版本可提供两份 Markdown：

```text
release-notes/v0.1.10/zh-CN.md
release-notes/v0.1.10/en.md
```

Release workflow 会将两份说明合并为同一个 GitHub Release 正文，并以可折叠的
“中文 / English”区块展示。应用内更新根据当前界面语言选择对应内容，缺失时回退
英文或 Release 原始正文。

不要为同一个版本创建两个 Release。版本比较、`releases/latest` 和更新资产匹配都要求
一个版本只有一个 tag。

## 发布产物

更新清单 `latest.json` 包含：

- 最新稳定版本与发布时间。
- 当前版本之后的 Release 注释，可跨多个版本聚合展示。
- macOS arm64/x64、Windows x64、Linux x64 的更新包 URL、大小、SHA-256 和类型。

`latest.json.sig` 是清单原始字节的 Ed25519 签名。应用先验证签名，再解析版本和资产。
下载完成后再次校验 SHA-256，失败包不会交给安装器。

当前免费发布策略：

- macOS：ad-hoc 签名，使用 `.app.tar.gz` 和退出后替换 helper。Gatekeeper 可能提示
  未知开发者；取得 Developer ID 后只需在 CI 中启用签名和公证。
- Windows：per-user Inno Setup，使用 `/UPDATE` 静默升级并重启。没有 Authenticode
  时可能出现 SmartScreen 提示；后续接入 SignPath 或其他证书不需要修改更新协议。
- Linux：下载 `.deb` 后通过 `pkexec` 或 `sudo` 请求授权，再重启应用。系统级安装无法
  在最小权限下完全静默。

## 发布流程

1. 保证 tag 与 `apps/fluxdb-desktop/Cargo.toml` 版本完全一致。
2. 准备中英 Release 说明。
3. dispatch 三平台 CI 并确认全绿。
4. 推送 `v*` tag。
5. workflow 构建安装包、生成清单和签名，并上传到同一个 Release。

未配置 `FLUXDB_UPDATE_SIGNING_KEY` 时 workflow 会发警告并跳过 `latest.json`，不会发布
一个可被篡改的更新源。

## 本地验证

debug 构建可通过 `FLUXDB_UPDATE_MANIFEST_URL` 和 `FLUXDB_UPDATE_SIGNATURE_URL` 指向
本地 HTTP 测试源，同时使用测试公钥编译：

```sh
FLUXDB_UPDATE_PUBLIC_KEY="<base64>" \
FLUXDB_UPDATE_MANIFEST_URL="http://127.0.0.1:8000/latest.json" \
FLUXDB_UPDATE_SIGNATURE_URL="http://127.0.0.1:8000/latest.json.sig" \
cargo run -p fluxdb-desktop
```

生产构建只接受 HTTPS 更新包地址。
