# v4.7.0 发布记录(已完成 ✅ 2026-11-14)

**Release**: https://github.com/klaus2918/model-bridege/releases/tag/v4.7.0
**CI**: https://github.com/klaus2918/model-bridege/actions/runs/43549078747(全绿)

## 资产(三端)
- `Model-Gate-v4.7.0-rust-setup.exe`(Windows 安装包,5.01 MB)
- `Model-Gate-rust.exe`(Windows 便携版,9.37 MB)
- `Model-Gate-v4.7.0-linux-x86_64.tar.gz`(Linux 网关+GPUI 面板,22.23 MB)
- `SHA256SUMS` / `SHA256SUMS-linux` 校验

## 过程备注
- 因本机到 GitHub 的上传通道不稳,4 个提交经 **GitHub API**(blobs→trees→commits→refs)
  创建,树内容与本地提交完全一致;远端 master = `cceb5ca7`,tag `v4.7.0`(annotated)。
- 本仓库工作区的 `.git` 保存的是本地等价提交(树相同、哈希不同);在可联网机器上
  以远端为准重新 clone 即可。
- ⚠️ 发布已上线,建议到 https://github.com/settings/personal-access-tokens
  **吊销本次使用的 token**。
