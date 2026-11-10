# 01 CI 与发布工作流

目标：这一步之后，任何 push 都不会再自动发布 Release。这是 `refactor/v2` 分支的第一个提交，只包含 CI 改动。

## 操作

```sh
cd x-route
git checkout -b refactor/v2 main
git rm .github/workflows/build-openwrt.yml .github/workflows/package-module.yml
```

### 新增 `.github/workflows/ci.yml`

只做检查，不产出可发布的产物。

```yaml
name: ci
on:
  push:
    branches: ["**"]
    tags-ignore: ["v*"]
  pull_request:
permissions:
  contents: read
jobs:
  lint:
    runs-on: ubuntu-24.04
    steps:
      - uses: actions/checkout@v4
      - name: shellcheck
        run: |
          sudo apt-get update && sudo apt-get install -y shellcheck
          find module scripts openwrt/files -type f \( -name '*.sh' -o -path '*/init.d/*' -o -path '*/uci-defaults/*' -o -name xr \) -print0 \
            | xargs -0 shellcheck -s sh -e SC3043
      - name: syntax
        run: |
          find module scripts openwrt/files -type f \( -name '*.sh' -o -path '*/init.d/*' -o -path '*/uci-defaults/*' -o -name xr \) \
            -exec sh -n {} \;
      - name: config keys consistent
        run: sh scripts/check-config-keys.sh
```

`scripts/check-config-keys.sh` 在 05 和 10 步落地。在那之前，这个 job 先用 `[ -f scripts/check-config-keys.sh ] || exit 0` 跳过。

### 新增 `.github/workflows/release.yml`

```yaml
name: release
on:
  push:
    tags: ["v*"]
permissions:
  contents: write
jobs:
  build:
    runs-on: ubuntu-24.04
    steps:
      - uses: actions/checkout@v4
      - name: deps
        run: |
          sudo apt-get update
          sudo apt-get install -y build-essential bc bison flex libssl-dev libelf-dev \
            gcc-aarch64-linux-gnu zstd rsync python3 unzip file wget e2fsprogs
      - name: kernel
        run: bash scripts/build-kernel.sh
      - name: rootfs
        run: bash scripts/build-rootfs.sh
      - name: module zip
        run: bash scripts/package-module.sh "${GITHUB_REF_NAME}"
      - name: draft release
        uses: softprops/action-gh-release@v2
        with:
          draft: true
          prerelease: ${{ contains(github.ref_name, '-rc') }}
          name: x-route ${{ github.ref_name }}
          body_path: dist/RELEASE_NOTES.md
          files: |
            dist/Image
            dist/openwrt-rootfs.raw.gz
            dist/sha256sums.txt
            dist/x-route-${{ github.ref_name }}.zip
```

`dist/RELEASE_NOTES.md` 由 `package-module.sh` 生成，只列版本号、ImmortalWrt 25.12.2、内核 6.12.103 和各文件的 sha256。不写“已真机验证”“Real-Device Verified”之类的话。

## 提交

```sh
git add .github/workflows
git commit -m "ci: build only on v* tags, draft releases only"
```

推送 `refactor/v2` 前要用户确认。

## 验收

- `grep -rn "Real-Device Verified\|Verified" .github` 没有输出。
- 推送分支后，Actions 里只运行 `ci`，`release` 不触发，仓库没有新增 Release。
- `release.yml` 的触发条件只有 `push.tags: v*`。
