//! `arg0` helper crate
//!
//! 说明（中文）:
//! - 本 crate 提供一个通用的 "arg0 分发" helper：把单个可执行文件根据
//!   `argv[0]`（即可执行名）或第一个参数分发成若干不同的行为。这样可以
//!   通过一个二进制模拟部署多个可执行（例如 `apply_patch` /
//!   `codex-linux-sandbox`），简化安装与部署。
//! - 常见场景：用户把同一份二进制通过符号链接或硬链接命名为
//!   `apply_patch`，程序根据 `argv[0]` 直接执行对应逻辑。
//!
//! 同时，本文件也处理：从 `.env` 文件加载环境变量（但禁止以 `CODEX_`
//! 前缀写入以避免覆盖内部变量）、以及临时在 PATH 中放置一个小脚本或
//! 链接以便 `apply_patch` 可用（无需全局安装单独的 apply_patch 可执行）。
//!
//! 阅读提示（涉及的 Rust 特性）:
//! - 条件编译：`#[cfg(unix)]` / `#[cfg(windows)]` 用于根据目标平台包含代码块。
//! - unsafe: 在某些场景调用 `std::env::set_var` 被包裹为 `unsafe`，这通常是
//!   因为在多线程并发修改 env 的时候不安全，本文件在修改 env 前保证
//!   单线程执行。
//! - 异步启动：`arg0_dispatch_or_else` 接受一个 `async` 闭包来执行主入口，
//!   并在内部构造 Tokio runtime 去运行它（这允许上层 `main()` 仍可使用
//!   `?` 错误传播语法）。

use std::future::Future;
use std::path::Path;
use std::path::PathBuf;

use codex_core::CODEX_APPLY_PATCH_ARG1;
#[cfg(unix)]
use std::os::unix::fs::symlink;
use tempfile::TempDir;

// 常量：可执行名与别名
const LINUX_SANDBOX_ARG0: &str = "codex-linux-sandbox";
const APPLY_PATCH_ARG0: &str = "apply_patch";
const MISSPELLED_APPLY_PATCH_ARG0: &str = "applypatch";

/// While we want to deploy the Codex CLI as a single executable for simplicity,
/// we also want to expose some of its functionality as distinct CLIs, so we use
/// the "arg0 trick" to determine which CLI to dispatch. This effectively allows
/// us to simulate deploying multiple executables as a single binary on Mac and
/// Linux (but not Windows).
///
/// 这个函数的职责总结：
/// 1. 根据 `argv[0]` 识别是否通过 alias/hard-link 调用（例如 `codex-linux-sandbox`,
///    `apply_patch`）。若匹配，则直接执行对应的子程序并直接返回（或永不返回）。
/// 2. 检查第一个参数是否为 `--codex-run-as-apply-patch`（内部约定的 secret），
///    若是则把后续 PATCH 参数交给 `codex_apply_patch::apply_patch` 执行并退出。
/// 3. 在常规流程中，先加载 `.env`（但禁止修改 `CODEX_` 前缀的环境），
///    然后在 PATH 前置一个临时目录（包含 `apply_patch` 的链接/脚本），
///    再创建 Tokio 运行时并执行外部传入的 `main_fn`（async closure）。
///
/// 类型签名说明（泛型与异步）:
/// - `F: FnOnce(Option<PathBuf>) -> Fut` 表示 `main_fn` 是一个一次性调用的闭包，
///   接受一个 `Option<PathBuf>` 参数并返回一个 Future（`Fut`）。
/// - `Fut: Future<Output = anyhow::Result<()>>` 表示该 Future 最终 `await` 后
///   会产生 `anyhow::Result<()>`，方便上层使用 `?` 做错误传播。
pub fn arg0_dispatch_or_else<F, Fut>(main_fn: F) -> anyhow::Result<()>
where
    F: FnOnce(Option<PathBuf>) -> Fut,
    Fut: Future<Output = anyhow::Result<()>>,
{
    // 读取原始命令行参数（OsString 以避免编码问题）
    let mut args = std::env::args_os();
    let argv0 = args.next().unwrap_or_default();
    // 取出 exe 的文件名部分作为识别依据（例如 /usr/local/bin/app -> app）
    let exe_name = Path::new(&argv0)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("");

    // 如果通过特殊 alias 启动，则直接进入对应子程序（这些分支可能永不返回）
    if exe_name == LINUX_SANDBOX_ARG0 {
        // Safety: [`run_main`] never returns (it takes control and runs the sandbox loop).
        codex_linux_sandbox::run_main();
    } else if exe_name == APPLY_PATCH_ARG0 || exe_name == MISSPELLED_APPLY_PATCH_ARG0 {
        // 通过 alias 调用 apply_patch 子程序
        codex_apply_patch::main();
    }

    // 检查第一个参数是否为内部约定的 apply-patch 标识（例如 --codex-run-as-apply-patch）
    let argv1 = args.next().unwrap_or_default();
    if argv1 == CODEX_APPLY_PATCH_ARG1 {
        // 这是一个轻量的子命令模式：直接把后续的 PATCH 参数传给 apply_patch 并退出
        let patch_arg = args.next().and_then(|s| s.to_str().map(|s| s.to_owned()));
        let exit_code = match patch_arg {
            Some(patch_arg) => {
                let mut stdout = std::io::stdout();
                let mut stderr = std::io::stderr();
                match codex_apply_patch::apply_patch(&patch_arg, &mut stdout, &mut stderr) {
                    Ok(()) => 0,
                    Err(_) => 1,
                }
            }
            None => {
                eprintln!("Error: {CODEX_APPLY_PATCH_ARG1} requires a UTF-8 PATCH argument.");
                1
            }
        };
        std::process::exit(exit_code);
    }

    // 在创建任何线程或 Tokio 运行时之前，加载 .env 环境变量（因为修改环境变量在多线程下不安全）
    load_dotenv();

    // 在 PATH 前加入一个临时目录（包含 apply_patch 的链接/脚本），并保留 TempDir
    // 以确保临时目录在函数作用域内有效（函数结束时 TempDir 会被删除）。
    let _path_entry = match prepend_path_entry_for_apply_patch() {
        Ok(path_entry) => Some(path_entry),
        Err(err) => {
            // 非致命错误：如果无法更新 PATH，仍然可以继续运行，但告警用户
            eprintln!("WARNING: proceeding, even though we could not update PATH: {err}");
            None
        }
    };

    // 常规路径：构造一个 Tokio runtime 并执行异步 main_fn。
    // 注意：这里使用 block_on 在当前线程上等待异步完成，保持主流程简单。
    let runtime = tokio::runtime::Runtime::new()?;
    runtime.block_on(async move {
        // 仅在 Linux 平台将当前 exe 路径传递给子任务（用于 spawn sandbox）
        let codex_linux_sandbox_exe: Option<PathBuf> = if cfg!(target_os = "linux") {
            std::env::current_exe().ok()
        } else {
            None
        };

        main_fn(codex_linux_sandbox_exe).await
    })
}

const ILLEGAL_ENV_VAR_PREFIX: &str = "CODEX_";

/// Load env vars from ~/.codex/.env and `$(pwd)/.env`.
///
/// Security: Do not allow `.env` files to create or modify any variables
/// with names starting with `CODEX_`.
///
/// 说明：使用 `dotenvy` 来逐条读取环境变量并通过 `set_filtered` 过滤后设置。
fn load_dotenv() {
    if let Ok(codex_home) = codex_core::config::find_codex_home()
        && let Ok(iter) = dotenvy::from_path_iter(codex_home.join(".env"))
    {
        set_filtered(iter);
    }

    if let Ok(iter) = dotenvy::dotenv_iter() {
        set_filtered(iter);
    }
}

/// Helper to set vars from a dotenvy iterator while filtering out `CODEX_` keys.
///
/// 细节说明：
/// - `IntoIterator<Item = Result<(String, String), dotenvy::Error>>` 表示迭代器
///   每一项是一个 Result，先用 `flatten()` 跳过错误项。
/// - 之所以用 `unsafe { std::env::set_var(...) }` 是为了明确说明我们在单线程
///   上下文设置 env，这在多线程并发修改 env 的场景下会是不安全的。
fn set_filtered<I>(iter: I)
where
    I: IntoIterator<Item = Result<(String, String), dotenvy::Error>>,
{
    for (key, value) in iter.into_iter().flatten() {
        if !key.to_ascii_uppercase().starts_with(ILLEGAL_ENV_VAR_PREFIX) {
            // It is safe to call set_var() because our process is
            // single-threaded at this point in its execution.
            unsafe { std::env::set_var(&key, &value) };
        }
    }
}

/// Creates a temporary directory with either:
///
/// - UNIX: `apply_patch` symlink to the current executable
/// - WINDOWS: `apply_patch.bat` batch script to invoke the current executable
///   with the "secret" --codex-run-as-apply-patch flag.
///
/// This temporary directory is prepended to the PATH environment variable so
/// that `apply_patch` can be on the PATH without requiring the user to
/// install a separate `apply_patch` executable, simplifying the deployment of
/// Codex CLI.
///
/// IMPORTANT: This function modifies the PATH environment variable, so it MUST
/// be called before multiple threads are spawned.
fn prepend_path_entry_for_apply_patch() -> std::io::Result<TempDir> {
    let temp_dir = TempDir::new()?;
    let path = temp_dir.path();

    for filename in &[APPLY_PATCH_ARG0, MISSPELLED_APPLY_PATCH_ARG0] {
        let exe = std::env::current_exe()?;

        #[cfg(unix)]
        {
            // 在 UNIX 上创建一个符号链接指向当前可执行文件，这样 PATH 中的
            // `apply_patch` 就会调用本程序，并且会根据 `argv[0]` 分发到 apply 子逻辑。
            let link = path.join(filename);
            symlink(&exe, &link)?;
        }

        #[cfg(windows)]
        {
            // Windows 环境下无法使用 POSIX symlink 方式，这里写入一个批处理脚本
            // 转发到当前 exe 并附带特殊标识符参数以让程序识别。
            let batch_script = path.join(format!("{filename}.bat"));
            std::fs::write(
                &batch_script,
                format!(
                    r#"@echo off
"{}" {CODEX_APPLY_PATCH_ARG1} %*
"#,
                    exe.display()
                ),
            )?;
        }
    }

    // 不同平台的 PATH 分隔符
    #[cfg(unix)]
    const PATH_SEPARATOR: &str = ":";

    #[cfg(windows)]
    const PATH_SEPARATOR: &str = ";";

    let path_element = path.display();
    let updated_path_env_var = match std::env::var("PATH") {
        Ok(existing_path) => {
            format!("{path_element}{PATH_SEPARATOR}{existing_path}")
        }
        Err(_) => {
            format!("{path_element}")
        }
    };

    // 更新环境变量，同样在单线程上下文内进行以避免竞态。
    unsafe {
        std::env::set_var("PATH", updated_path_env_var);
    }

    Ok(temp_dir)
}
