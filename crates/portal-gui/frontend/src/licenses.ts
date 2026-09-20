// 开源软件声明数据。
// 名称、许可证与仓库地址取自本地依赖元数据（node_modules 的 package.json 与
// Cargo registry 中的 Cargo.toml），仅列出用户可感知的主要依赖。升级依赖后请同步更新。
export interface LicenseEntry {
  name: string
  license: string
  url: string
  summary: string
}

export interface LicenseGroup {
  title: string
  entries: LicenseEntry[]
}

export const licenseNotice = 'DGCU Portal 以 GPL-3.0-only 发布，并使用下列开源项目。点击任意条目在系统浏览器中打开其仓库。这里只列出主要依赖，完整依赖树可以用 cargo license 与 pnpm licenses list 生成。'

export const licenseGroups: LicenseGroup[] = [
  {
    title: '桌面与界面',
    entries: [
      { name: 'Tauri', license: 'Apache-2.0 OR MIT', url: 'https://github.com/tauri-apps/tauri', summary: '桌面窗口、系统托盘与前后端 IPC 桥' },
      { name: 'Vue 3', license: 'MIT', url: 'https://github.com/vuejs/core', summary: '界面框架与响应式状态' },
      { name: 'miuix-vue', license: 'Apache-2.0', url: 'https://github.com/YuKongA/miuix-vue', summary: '界面组件与主题变量' },
      { name: 'motion-v', license: 'MIT', url: 'https://github.com/motiondivision/motion-vue', summary: '页面切换与动画过渡' }
    ]
  },
  {
    title: '认证与网络核心',
    entries: [
      { name: 'reqwest', license: 'MIT OR Apache-2.0', url: 'https://github.com/seanmonstar/reqwest', summary: 'HTTP 客户端，使用 rustls 而非 OpenSSL' },
      { name: 'tokio', license: 'MIT', url: 'https://github.com/tokio-rs/tokio', summary: '异步运行时与定时任务' },
      { name: 'serde / serde_json', license: 'MIT OR Apache-2.0', url: 'https://github.com/serde-rs/serde', summary: '设置与快照的序列化' },
      { name: 'url', license: 'MIT OR Apache-2.0', url: 'https://github.com/servo/rust-url', summary: 'Portal 地址解析与校验' },
      { name: 'dom_query', license: 'MIT', url: 'https://github.com/niklak/dom_query', summary: '认证页面 DOM 解析' },
      { name: 'regex', license: 'MIT OR Apache-2.0', url: 'https://github.com/rust-lang/regex', summary: 'Portal 响应字段提取' },
      { name: 'network-interface', license: 'MIT OR Apache-2.0', url: 'https://github.com/EstebanBorai/network-interface', summary: '读取本机网卡的 IPv4 与 MAC' }
    ]
  },
  {
    title: '进程、命令行与本地存储',
    entries: [
      { name: 'interprocess', license: '0BSD OR Apache-2.0', url: 'https://github.com/kotauskas/interprocess', summary: 'GUI 与 CLI 之间的本地 IPC' },
      { name: 'tauri-plugin-single-instance', license: 'Apache-2.0 OR MIT', url: 'https://github.com/tauri-apps/plugins-workspace', summary: '单实例运行，重复启动时唤起已有窗口' },
      { name: 'clap', license: 'MIT OR Apache-2.0', url: 'https://github.com/clap-rs/clap', summary: 'DGCU CLI 的命令行解析' },
      { name: 'rpassword', license: 'Apache-2.0', url: 'https://github.com/conradkleinespel/rpassword', summary: 'CLI 隐藏输入密码' },
      { name: 'directories', license: 'MIT OR Apache-2.0', url: 'https://github.com/soc/directories-rs', summary: '跨平台配置与数据目录' },
      { name: 'keyring', license: 'MIT OR Apache-2.0', url: 'https://github.com/hwchen/keyring-rs', summary: '把密码交给系统凭据库保存' },
      { name: 'zeroize', license: 'Apache-2.0 OR MIT', url: 'https://github.com/RustCrypto/utils', summary: '退出时擦除内存中的凭据' },
      { name: 'thiserror', license: 'MIT OR Apache-2.0', url: 'https://github.com/dtolnay/thiserror', summary: '认证错误的类型化定义' },
      { name: 'webbrowser', license: 'MIT OR Apache-2.0', url: 'https://github.com/amodm/webbrowser-rs', summary: '用系统默认浏览器打开认证后台' }
    ]
  },
  {
    title: '构建与测试',
    entries: [
      { name: 'Vite', license: 'MIT', url: 'https://github.com/vitejs/vite', summary: '前端构建与开发服务器' },
      { name: '@vitejs/plugin-vue', license: 'MIT', url: 'https://github.com/vitejs/vite-plugin-vue', summary: 'Vite 的 Vue 单文件组件支持' },
      { name: 'TypeScript', license: 'Apache-2.0', url: 'https://github.com/microsoft/TypeScript', summary: '前端类型检查' },
      { name: 'vue-tsc', license: 'MIT', url: 'https://github.com/vuejs/language-tools', summary: 'Vue 模板类型检查' },
      { name: 'Vitest', license: 'MIT', url: 'https://github.com/vitest-dev/vitest', summary: '前端单元测试' }
    ]
  }
]
