use std::{
    fs,
    io::{stdout, BufWriter},
    path::PathBuf,
    process::Command,
};

use shrs::{
    history::FileBackedHistory,
    keybindings,
    prelude::{styled_buf::StyledBuf, *},
};
use shrs_cd_stack::{CdStackPlugin, CdStackState};
use shrs_cd_tools::git;
use shrs_command_timer::{CommandTimerPlugin, CommandTimerState};
use shrs_file_logger::{FileLogger, LevelFilter};
use shrs_mux::{python::*, BashLang, MuxHighlighter, MuxPlugin, MuxState, NuLang};
use shrs_output_capture::OutputCapturePlugin;
use shrs_rhai::RhaiPlugin;
use shrs_rhai_completion::CompletionsPlugin;
use shrs_run_context::RunContextPlugin;

// =-=-= Prompt customization =-=-=
// Create a new struct and implement the [Prompt] trait
struct MyPrompt;

impl Prompt for MyPrompt {
    fn prompt_left(&self, state: &LineStateBundle) -> StyledBuf {
        let indicator = match state.line.mode() {
            LineMode::Insert => String::from(">").cyan(),
            LineMode::Normal => String::from(":").yellow(),
        };
        if !state.line.lines.is_empty() {
            return styled_buf! {" ", indicator, " "};
        }

        styled_buf!(
            " ",
            username().map(|u| u.blue()),
            " ",
            top_pwd().white().bold(),
            " ",
            indicator,
            " "
        )
    }
    fn prompt_right(&self, line_ctx: &LineStateBundle) -> StyledBuf {
        let time_str = line_ctx
            .ctx
            .state
            .get::<CommandTimerState>()
            .and_then(|x| x.command_time())
            .map(|x| format!("{x:?}"));

        let lang_name = line_ctx
            .ctx
            .state
            .get::<MuxState>()
            .map(|state| state.current_lang())
            .expect("MuxState should be provided")
            .name();

        if !line_ctx.line.lines.is_empty() {
            return styled_buf!("");
        }
        if let Ok(git_branch) = git::branch().map(|s| format!("git:{s}").blue().bold()) {
            styled_buf!(git_branch, " ", time_str, " ", lang_name, " ")
        } else {
            styled_buf!(time_str, " ", lang_name, " ")
        }
    }
}

fn main() {
    let logger = FileLogger {
        path: PathBuf::from("/tmp/shrs_log"),
        level: LevelFilter::Debug,
    };

    logger.init().expect("Failed initializing file logger");

    let _out = BufWriter::new(stdout());

    // =-=-= Configuration directory =-=-=
    // Initialize the directory we will be using to hold our configuration and metadata files
    let config_dir = dirs::home_dir().unwrap().as_path().join(".config/shrs");
    // also log when creating dir
    // TODO ignore errors for now (we dont care if dir already exists)
    let _d = fs::create_dir_all(config_dir.clone());

    // =-=-= Environment variables =-=-=
    // Load environment variables from calling shell
    let mut env = Env::default();
    env.load().expect("Couldn't load env");
    env.set("SHELL_NAME", "shrs_example")
        .expect("Couldnt set env var");

    let builtins = Builtins::default();

    // =-=-= Completion =-=-=
    // Get list of binaries in path and initialize the completer to autocomplete command names
    let path_string = env.get("PATH").unwrap().to_string();
    let mut completer = DefaultCompleter::default();
    completer.register(Rule::new(
        Pred::new(cmdname_pred),
        Box::new(cmdname_action(path_string)),
    ));
    completer.register(Rule::new(
        Pred::new(cmdname_pred),
        Box::new(builtin_cmdname_action(&builtins)),
    ));

    // =-=-= Menu =-=-=-=
    let menu = DefaultMenu::default();

    // =-=-= History =-=-=
    // Use history that writes to file on disk
    let history_file = config_dir.as_path().join("history");
    let history = FileBackedHistory::new(history_file).expect("Could not open history file");

    // =-=-= Keybindings =-=-=
    // Add basic keybindings
    let keybinding = keybindings! {
        |state|
        "C-l" => ("Clear the screen", { Command::new("clear").spawn().expect("Couldn't clear screen")}),
        "C-p" => ("Move up one in the command history", {
            if let Some(cd_state) = state.ctx.state.get_mut::<CdStackState>() {
                if let Some(new_path) = cd_state.down() {
                    set_working_dir(state.sh, state.ctx, state.rt, &new_path, false).unwrap();
                }
            }
        }),
        "C-n" => ("Move down one in the command history", {
            if let Some(cd_state) = state.ctx.state.get_mut::<CdStackState>() {
                if let Some(new_path) = cd_state.up() {
                    set_working_dir(state.sh, state.ctx, state.rt, &new_path, false).unwrap();
                }
            }
        }),
    };

    // =-=-= Prompt =-=-=
    let prompt = MyPrompt;

    // =-=-= Readline =-=-=
    // Initialize readline with all of our components

    let mut snippets = Snippets::new(ExpandSnippet::OnSpace);
    snippets.add(
        "gc".to_string(),
        SnippetInfo::new("git commit -m \"", Position::Command),
    );
    snippets.add(
        "ga".to_string(),
        SnippetInfo::new("git add .", Position::Command),
    );

    let readline = LineBuilder::default()
        .with_menu(menu)
        .with_prompt(prompt)
        .with_highlighter(MuxHighlighter {})
        .with_snippets(snippets)
        .build()
        .expect("Could not construct readline");

    // =-=-= Aliases =-=-=
    // Set aliases
    let alias = Alias::from_iter([
        ("ls", "ls --color=auto"),
        ("l", "ls --color=auto"),
        ("c", "cd"),
        ("g", "git"),
        ("v", "vim"),
        ("V", "nvim"),
        ("la", "ls -a --color=auto"),
    ]);

    // =-=-= Hooks =-=-=
    // Create a hook that prints a welcome message on startup
    let startup_msg: HookFn<StartupCtx> = |_sh: &Shell,
                                           _sh_ctx: &mut Context,
                                           _sh_rt: &mut Runtime,
                                           _ctx: &StartupCtx|
     -> anyhow::Result<()> {
        let welcome_str = format!(
            r#"
        __
   ___ / /  _______
  (_-</ _ \/ __(_-<
 /___/_//_/_/ /___/
a rusty POSIX shell | build {}"#,
            env!("SHRS_VERSION")
        );

        println!("{welcome_str}");
        Ok(())
    };
    let mut hooks = Hooks::new();
    hooks.insert(startup_msg);

    // =-=-= Plugins =-=-=
    let mux_plugin = MuxPlugin::new()
        .register_lang("bash", BashLang::new())
        .register_lang("python", PythonLang::new())
        .register_theme("python", Box::new(PythonTheme::new()))
        .register_lang("nu", NuLang::new());

    // =-=-= Shell =-=-=
    // Construct the final shell
    let myshell = ShellBuilder::default()
        .with_completer(completer)
        .with_hooks(hooks)
        .with_env(env)
        .with_alias(alias)
        .with_readline(readline)
        .with_history(history)
        .with_keybinding(keybinding)
        .with_plugin(OutputCapturePlugin)
        .with_plugin(CommandTimerPlugin)
        .with_plugin(RunContextPlugin::default())
        .with_plugin(mux_plugin)
        .with_plugin(CdStackPlugin)
        .with_plugin(RhaiPlugin)
        .with_plugin(CompletionsPlugin)
        .build()
        .expect("Could not construct shell");

    myshell.run().unwrap();
}
