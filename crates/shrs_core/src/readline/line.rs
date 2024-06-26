//! Core readline configuration

use std::{
    borrow::BorrowMut,
    io::{Read, Seek, Write},
};

use ::crossterm::{
    cursor::SetCursorStyle,
    event::{
        read, DisableBracketedPaste, EnableBracketedPaste, Event, KeyCode, KeyEvent, KeyModifiers,
    },
    execute,
    style::{Color, ContentStyle},
    terminal::{disable_raw_mode, enable_raw_mode},
};
use shrs_utils::cursor_buffer::{CursorBuffer, Location};
use shrs_vi::{Action, Command, Motion, Parser};

use super::{painter::Painter, *};
use crate::{
    prelude::{Completion, CompletionCtx, ReplaceMethod},
    shell::{Context, Runtime, Shell},
};

pub trait Readline {
    fn read_line(&mut self, sh: &Shell, ctx: &mut Context, rt: &mut Runtime) -> String;
}

/// Operating mode of readline
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum LineMode {
    /// Vi insert mode
    Insert,
    /// Vi normal mode
    Normal,
}

/// State or where the prompt is in history browse mode
#[derive(Debug, PartialEq, Eq)]
pub enum HistoryInd {
    /// Brand new prompt
    Prompt,
    /// In history line
    Line(usize),
}

impl HistoryInd {
    /// Go up (less recent) in history, if in prompt mode, then enter history
    pub fn up(&self, limit: usize) -> HistoryInd {
        match self {
            HistoryInd::Prompt => {
                if limit == 0 {
                    HistoryInd::Prompt
                } else {
                    HistoryInd::Line(0)
                }
            },
            HistoryInd::Line(i) => HistoryInd::Line((i + 1).min(limit - 1)),
        }
    }

    /// Go down (more recent) in history, if in most recent history line, enter prompt mode
    pub fn down(&self) -> HistoryInd {
        match self {
            HistoryInd::Prompt => HistoryInd::Prompt,
            HistoryInd::Line(i) => {
                if *i == 0 {
                    HistoryInd::Prompt
                } else {
                    HistoryInd::Line(i.saturating_sub(1))
                }
            },
        }
    }
}

/// State needed for readline
pub struct LineState {
    /// Cursor buffer structure for interactive editing
    pub cb: CursorBuffer,
    /// stored lines in a multiprompt command
    pub lines: String,

    // TODO this is temp, find better way to store prefix of current word
    current_word: String,
    // TODO dumping history index here for now
    history_ind: HistoryInd,
    /// Line contents that were present before entering history mode
    saved_line: String,
    /// The current mode the line is in
    mode: LineMode,
}

impl LineState {
    pub fn new() -> Self {
        LineState {
            cb: CursorBuffer::default(),
            current_word: String::new(),
            history_ind: HistoryInd::Prompt,
            saved_line: String::new(),
            mode: LineMode::Insert,
            lines: String::new(),
        }
    }

    pub fn mode(&self) -> LineMode {
        self.mode
    }

    /// Get the contents of the prompt
    pub fn get_full_command(&self) -> String {
        let mut res: String = self.lines.clone();
        let cur_line: String = self.cb.as_str().into();
        res += cur_line.as_str();

        res
    }
}

/// Context that is passed to [Line]
pub struct LineStateBundle<'a> {
    pub sh: &'a Shell,
    pub ctx: &'a mut Context,
    pub rt: &'a mut Runtime,
    pub line: &'a mut LineState,
}

impl<'a> LineStateBundle<'a> {
    pub fn new(
        sh: &'a Shell,
        ctx: &'a mut Context,
        rt: &'a mut Runtime,
        line: &'a mut LineState,
    ) -> Self {
        LineStateBundle { sh, ctx, rt, line }
    }
}

/// Configuration for readline
#[derive(Builder)]
#[builder(pattern = "owned")]
#[builder(setter(prefix = "with"))]
pub struct Line {
    /// Completion menu, see [Menu]
    #[builder(default = "Box::new(DefaultMenu::default())")]
    #[builder(setter(custom))]
    menu: Box<dyn Menu<MenuItem = Completion, PreviewItem = String>>,

    #[builder(default = "Box::new(DefaultBufferHistory::default())")]
    #[builder(setter(custom))]
    buffer_history: Box<dyn BufferHistory>,

    /// Syntax highlighter, see [Highlighter]
    #[builder(default = "Box::new(SyntaxHighlighter::default())")]
    #[builder(setter(custom))]
    highlighter: Box<dyn Highlighter>,

    /// Custom prompt, see [Prompt]
    #[builder(default = "Box::new(DefaultPrompt::default())")]
    #[builder(setter(custom))]
    prompt: Box<dyn Prompt>,

    // ignored fields
    #[builder(default = "Painter::default()")]
    #[builder(setter(skip))]
    painter: Painter,

    /// Currently pressed keys in normal mode
    #[builder(default = "String::new()")]
    #[builder(setter(skip))]
    normal_keys: String,

    #[builder(default = "Box::new(DefaultSuggester)")]
    suggester: Box<dyn Suggester>,

    /// Alias expansions, see [Abbreviations]
    #[builder(default = "Snippets::default()")]
    snippets: Snippets,
}

impl Default for Line {
    fn default() -> Self {
        LineBuilder::default().build().unwrap()
    }
}

// TODO none of the builder stuff is being autogenerated rn :()
impl LineBuilder {
    pub fn with_menu(
        mut self,
        menu: impl Menu<MenuItem = Completion, PreviewItem = String> + 'static,
    ) -> Self {
        self.menu = Some(Box::new(menu));
        self
    }
    pub fn with_highlighter(mut self, highlighter: impl Highlighter + 'static) -> Self {
        self.highlighter = Some(Box::new(highlighter));
        self
    }
    pub fn with_prompt(mut self, prompt: impl Prompt + 'static) -> Self {
        self.prompt = Some(Box::new(prompt));
        self
    }
}

impl Readline for Line {
    /// Start readline and read one line of user input
    fn read_line(&mut self, sh: &Shell, ctx: &mut Context, rt: &mut Runtime) -> String {
        let mut line_state = LineState::new();
        let mut state_bundle = LineStateBundle::new(sh, ctx, rt, &mut line_state);
        self.read_events(&mut state_bundle).unwrap()
    }
}

impl Line {
    fn read_events(&mut self, state: &mut LineStateBundle) -> anyhow::Result<String> {
        // ensure we are always cleaning up whenever we leave this scope
        struct CleanUp;
        impl Drop for CleanUp {
            fn drop(&mut self) {
                let _ = disable_raw_mode();
                let _ = execute!(std::io::stdout(), DisableBracketedPaste);
            }
        }
        let _cleanup = CleanUp;

        enable_raw_mode()?;
        execute!(std::io::stdout(), EnableBracketedPaste)?;

        let mut auto_run = false;
        self.painter.init().unwrap();
        if let Some(c) = state.ctx.prompt_content_queue.pop() {
            auto_run = c.auto_run;
            state
                .line
                .cb
                .insert(Location::Cursor(), c.content.as_str())?;
        }

        loop {
            let res = state.line.get_full_command();

            // syntax highlight
            let mut styled_buf = self
                .highlighter
                .highlight(state, &res)
                .slice_from(state.line.lines.len());

            // add currently selected completion to buf
            if self.menu.is_active() {
                if let Some(selection) = self.menu.current_selection() {
                    let trimmed_selection = &selection.accept()[state.line.current_word.len()..];
                    styled_buf.push(
                        trimmed_selection,
                        ContentStyle {
                            foreground_color: Some(Color::Red),
                            ..Default::default()
                        },
                    );
                }
            } else {
                // get search results from history and suggest the first result
                if let Some(suggestion) = self.suggester.suggest(state) {
                    let trimmed_selection = suggestion[res.len()..].to_string();
                    styled_buf.push(trimmed_selection.as_str(), state.sh.theme.suggestion_style);
                }
            }

            self.painter.paint(
                state,
                &self.prompt,
                &self.menu,
                &styled_buf,
                state.line.cb.cursor(),
            )?;
            if auto_run {
                self.buffer_history.clear();
                self.painter.newline()?;
                break;
            }

            let event = read()?;

            if let Event::Key(key_event) = event {
                if state.sh.keybinding.handle_key_event(state, key_event) {
                    break;
                }
            }

            let should_break = self.handle_standard_keys(state, event.clone())?;
            if should_break {
                break;
            }

            // handle menu events
            if self.menu.is_active() {
                self.handle_menu_keys(state, event.clone())?;
            } else {
                match state.line.mode {
                    LineMode::Insert => {
                        self.handle_insert_keys(state, event)?;
                    },
                    LineMode::Normal => {
                        self.handle_normal_keys(state, event)?;
                    },
                }
            }
        }

        let res = state.line.get_full_command();
        if !res.is_empty() {
            state.ctx.history.add(res.clone());
        }
        Ok(res)
    }

    fn handle_menu_keys(&mut self, ctx: &mut LineStateBundle, event: Event) -> anyhow::Result<()> {
        match event {
            Event::Key(KeyEvent {
                code: KeyCode::Enter,
                modifiers: KeyModifiers::NONE,
                ..
            }) => {
                if let Some(accepted) = self.menu.accept().cloned() {
                    self.accept_completion(ctx, accepted)?;
                }
            },
            Event::Key(KeyEvent {
                code: KeyCode::Esc,
                modifiers: KeyModifiers::NONE,
                ..
            }) => {
                self.menu.disactivate();
            },
            Event::Key(KeyEvent {
                code: KeyCode::Tab,
                modifiers: KeyModifiers::SHIFT,
                ..
            })
            | Event::Key(KeyEvent {
                code: KeyCode::Up,
                modifiers: KeyModifiers::NONE,
                ..
            }) => {
                self.menu.previous();
            },
            Event::Key(KeyEvent {
                code: KeyCode::Tab,
                modifiers: KeyModifiers::NONE,
                ..
            })
            | Event::Key(KeyEvent {
                code: KeyCode::Down,
                modifiers: KeyModifiers::NONE,
                ..
            }) => {
                self.menu.next();
            },
            _ => {
                self.menu.disactivate();
                match ctx.line.mode {
                    LineMode::Insert => {
                        self.handle_insert_keys(ctx, event)?;
                    },
                    LineMode::Normal => {
                        self.handle_normal_keys(ctx, event)?;
                    },
                };
            },
        };
        Ok(())
    }

    //Keys that are universal regardless of mode, ex. Enter, Ctrl-c
    fn handle_standard_keys(
        &mut self,
        state: &mut LineStateBundle,
        event: Event,
    ) -> anyhow::Result<bool> {
        match event {
            Event::Resize(a, b) => {
                self.painter.set_term_size(a, b);
            },
            Event::Paste(p) => {
                state.line.cb.insert(Location::Cursor(), p.as_str())?;
            },
            Event::Key(KeyEvent {
                code: KeyCode::Char('c'),
                modifiers: KeyModifiers::CONTROL,
                ..
            }) => {
                state.line.cb.clear();
                self.buffer_history.clear();
                state.line.lines = String::new();
                self.painter.newline()?;

                return Ok(true);
            },
            // Insert suggestion when right arrow
            Event::Key(KeyEvent {
                code: KeyCode::Right,
                modifiers: KeyModifiers::NONE,
                ..
            }) => {
                if let Some(suggestion) = self.suggester.suggest(state) {
                    state.line.cb.clear();
                    state
                        .line
                        .cb
                        .insert(Location::Cursor(), suggestion.as_str())?;
                }
            },

            Event::Key(KeyEvent {
                code: KeyCode::Enter,
                modifiers: KeyModifiers::NONE,
                ..
            })
            | Event::Key(KeyEvent {
                code: KeyCode::Char('j'),
                modifiers: KeyModifiers::CONTROL,
                ..
            }) => {
                if self.menu.is_active() {
                    return Ok(false);
                }
                self.buffer_history.clear();
                self.painter.newline()?;

                if state.sh.lang.needs_line_check(state) {
                    state.line.lines += state.line.cb.as_str().into_owned().as_str();
                    state.line.lines += "\n";
                    state.line.cb.clear();

                    return Ok(false);
                }

                return Ok(true);
            },
            Event::Key(KeyEvent {
                code: KeyCode::Char('d'),
                modifiers: KeyModifiers::CONTROL,
                ..
            }) => {
                // if current input is empty exit the shell, otherwise treat it as enter
                if state.line.cb.is_empty() {
                    // TODO maybe unify exiting the shell
                    let _ = disable_raw_mode(); // TODO this is temp fix, should be more graceful way of
                                                // handling cleanup code
                    std::process::exit(0);
                } else {
                    self.buffer_history.clear();
                    self.painter.newline()?;
                    return Ok(true);
                }
            },

            _ => (),
        };

        Ok(false)
    }
    /// returns a bool whether input should still be handled
    pub fn expand(&mut self, state: &mut LineStateBundle, event: &Event) -> anyhow::Result<bool> {
        if !self.snippets.should_expand(event) {
            return Ok(true);
        }
        //find current word

        let cur_line = state.line.cb.as_str().to_string();
        let mut words = cur_line.split(' ').collect::<Vec<_>>();
        let mut char_offset = 0;
        //cursor is positioned just after the last typed character
        let index_before_cursor = state.line.cb.cursor();
        let mut cur_word_index = None;
        for (i, word) in words.iter().enumerate() {
            // Determine the start and end indices of the current word
            let start_index = char_offset;
            let end_index = char_offset + word.len();

            // Check if the cursor index falls within the current word
            if index_before_cursor >= start_index && index_before_cursor <= end_index {
                cur_word_index = Some(i);
            }

            // Update the character offset to account for the current word and whitespace
            char_offset = end_index + 1; // Add 1 for the space between words
        }

        if let Some(c) = cur_word_index {
            if let Some(expanded) = self.snippets.get(&words[c].to_string()) {
                //check if we're we're expanding the first word
                if expanded.position == Position::Command {
                    if c != 0 {
                        return Ok(true);
                    }
                }
                words[c] = expanded.value.as_str();

                state.line.cb.clear();
                //cursor automatically positioned at end
                state
                    .line
                    .cb
                    .insert(Location::Cursor(), words.join(" ").as_str())?;
                return Ok(false);
            }
        }
        return Ok(true);
    }

    fn handle_insert_keys(
        &mut self,
        state: &mut LineStateBundle,
        event: Event,
    ) -> anyhow::Result<()> {
        if !self.expand(state, &event)? {
            return Ok(());
        }

        match event {
            Event::Key(KeyEvent {
                code: KeyCode::Tab,
                modifiers: KeyModifiers::NONE,
                ..
            }) => {
                self.populate_completions(state)?;
                self.menu.activate();

                let completion_len = self.menu.items().len();

                // no-op if no completions
                if completion_len == 0 {
                    self.menu.disactivate();
                    return Ok(());
                }
                // if completions only has one entry, automatically select it
                if completion_len == 1 {
                    // TODO stupid ownership stuff
                    let item = self.menu.items().get(0).map(|x| (*x).clone()).unwrap();
                    self.accept_completion(state, item.1)?;
                    self.menu.disactivate();
                    return Ok(());
                }

                // TODO make this feature toggable
                // TODO this is broken
                // Automatically accept the common prefix
                /*
                let completions: Vec<&str> = self
                    .menu
                    .items()
                    .iter()
                    .map(|(preview, _)| preview.as_str())
                    .collect();
                let prefix = longest_common_prefix(completions);
                self.accept_completion(
                    ctx,
                    Completion {
                        add_space: false,
                        display: None,
                        completion: prefix.clone(),
                        replace_method: ReplaceMethod::Append,
                    },
                )?;

                // recompute completions with prefix stripped
                // TODO this code is horrifying
                let items = self.menu.items();
                let new_items = items
                    .iter()
                    .map(|(preview, complete)| {
                        let mut complete = complete.clone();
                        complete.completion = complete.completion[prefix.len()..].to_string();
                        (preview.clone(), complete)
                    })
                    .collect();
                self.menu.set_items(new_items);
                */

                self.menu.activate();
            },
            Event::Key(KeyEvent {
                code: KeyCode::Left,
                modifiers: KeyModifiers::NONE,
                ..
            }) => {
                if state.line.cb.cursor() > 0 {
                    state.line.cb.move_cursor(Location::Before())?;
                }
            },
            Event::Key(KeyEvent {
                code: KeyCode::Right,
                modifiers: KeyModifiers::NONE,
                ..
            }) => {
                if state.line.cb.cursor() < state.line.cb.len() {
                    state.line.cb.move_cursor(Location::After())?;
                }
            },
            Event::Key(KeyEvent {
                code: KeyCode::Down,
                modifiers: KeyModifiers::NONE,
                ..
            }) => {
                self.history_down(state)?;
            },
            Event::Key(KeyEvent {
                code: KeyCode::Up,
                modifiers: KeyModifiers::NONE,
                ..
            }) => {
                self.history_up(state)?;
            },
            Event::Key(KeyEvent {
                code: KeyCode::Esc, ..
            }) => {
                self.to_normal_mode(state)?;
                self.buffer_history.add(&state.line.cb);
            },
            Event::Key(KeyEvent {
                code: KeyCode::Backspace,
                modifiers: KeyModifiers::NONE,
                ..
            })
            | Event::Key(KeyEvent {
                code: KeyCode::Char('h'),
                modifiers: KeyModifiers::CONTROL,
                ..
            }) => {
                if !state.line.cb.is_empty() && state.line.cb.cursor() != 0 {
                    state
                        .line
                        .cb
                        .delete(Location::Before(), Location::Cursor())?;
                }
            },
            Event::Key(KeyEvent {
                code: KeyCode::Char('w'),
                modifiers: KeyModifiers::CONTROL,
                ..
            }) => {
                if !state.line.cb.is_empty() && state.line.cb.cursor() != 0 {
                    let start = state.line.cb.motion_to_loc(Motion::BackWord)?;
                    state.line.cb.delete(start, Location::Cursor())?;
                }
            },

            Event::Key(KeyEvent {
                code: KeyCode::Char('a'),
                modifiers: KeyModifiers::CONTROL,
                ..
            }) => {
                state.line.cb.move_cursor(Location::Front())?;
            },

            Event::Key(KeyEvent {
                code: KeyCode::Char('e'),
                modifiers: KeyModifiers::CONTROL,
                ..
            }) => {
                state.line.cb.move_cursor(Location::Back(&state.line.cb))?;
            },

            Event::Key(KeyEvent {
                code: KeyCode::Char(c),
                ..
            }) => {
                state.line.cb.insert(Location::Cursor(), &c.to_string())?;
            },
            _ => {},
        };
        Ok(())
    }

    fn handle_normal_keys(
        &mut self,
        state: &mut LineStateBundle,
        event: Event,
    ) -> anyhow::Result<()> {
        // TODO write better system toString support key combinations
        match event {
            Event::Key(KeyEvent {
                code: KeyCode::Esc, ..
            }) => {
                self.normal_keys.clear();
            },
            Event::Key(KeyEvent {
                code: KeyCode::Char(c),
                ..
            }) => {
                self.normal_keys.push(c);

                if let Ok(Command { repeat, action }) = Parser::default().parse(&self.normal_keys) {
                    for _ in 0..repeat {
                        // special cases (possibly consulidate with execute_vi somehow)

                        if let Ok(mode) = state.line.cb.execute_vi(action.clone()) {
                            if mode != state.line.mode {
                                match mode {
                                    LineMode::Insert => self.to_insert_mode(state)?,
                                    LineMode::Normal => self.to_normal_mode(state)?,
                                };
                            }
                        }
                        match action {
                            Action::Undo => self.buffer_history.prev(state.line.cb.borrow_mut()),

                            Action::Redo => self.buffer_history.next(state.line.cb.borrow_mut()),
                            Action::Move(motion) => match motion {
                                Motion::Up => self.history_up(state)?,
                                Motion::Down => self.history_down(state)?,
                                _ => {},
                            },
                            Action::Editor => {
                                // TODO should this just use the env var? or should shrs have
                                // dedicated config?

                                // If EDITOR command is not set just display some sort of warning
                                // and move on
                                let Ok(editor) = std::env::var("EDITOR") else {
                                    return Ok(());
                                };

                                let mut tempbuf = tempfile::NamedTempFile::new().unwrap();

                                // write contexts of line to file
                                tempbuf
                                    .write_all(state.line.cb.as_str().as_bytes())
                                    .unwrap();

                                // TODO should use shrs_job for this?
                                // TODO configure the command used
                                let mut child = std::process::Command::new(editor)
                                    .arg(tempbuf.path())
                                    .spawn()
                                    .unwrap();

                                child.wait().unwrap();

                                // read update file contexts back to line
                                let mut new_contents = String::new();
                                tempbuf.rewind().unwrap();
                                tempbuf.read_to_string(&mut new_contents).unwrap();

                                // strip last newline
                                // TODO this is very platform and editor dependent
                                let trimmed = new_contents.trim_end_matches("\n");

                                state.line.cb.clear();
                                state.line.cb.insert(Location::Cursor(), trimmed).unwrap();

                                // TODO should auto run the command?

                                tempbuf.close().unwrap();
                            },
                            _ => {
                                self.buffer_history.add(&state.line.cb);
                            },
                        }
                    }

                    self.normal_keys.clear();
                }
            },
            _ => {},
        }
        Ok(())
    }

    // recalculate the current completions
    fn populate_completions(&mut self, state: &mut LineStateBundle) -> anyhow::Result<()> {
        // TODO IFS
        let args = state
            .line
            .cb
            .slice(..state.line.cb.cursor())
            .as_str()
            .unwrap()
            .split(' ');
        state.line.current_word = args.clone().last().unwrap_or("").to_string();

        let comp_ctx = CompletionCtx::new(args.map(|s| s.to_owned()).collect::<Vec<_>>());

        let completions = state.ctx.completer.complete(&comp_ctx);
        let completions = completions.iter().collect::<Vec<_>>();

        let menuitems = completions
            .iter()
            .map(|c| (c.display(), (*c).clone()))
            .collect::<Vec<_>>();
        self.menu.set_items(menuitems);

        Ok(())
    }

    // replace word at cursor with accepted word (used in automcompletion)
    fn accept_completion(
        &mut self,
        state: &mut LineStateBundle,
        completion: Completion,
    ) -> anyhow::Result<()> {
        // first remove current word
        // TODO could implement a delete_before
        // TODO make use of ReplaceMethod
        match completion.replace_method {
            ReplaceMethod::Append => {
                // no-op
            },
            ReplaceMethod::Replace => {
                state
                    .line
                    .cb
                    .move_cursor(Location::Rel(-(state.line.current_word.len() as isize)))?;
                let cur_word_len =
                    unicode_width::UnicodeWidthStr::width(state.line.current_word.as_str());
                state
                    .line
                    .cb
                    .delete(Location::Cursor(), Location::Rel(cur_word_len as isize))?;
                state.line.current_word.clear();
            },
        }

        // then replace with the completion word
        state
            .line
            .cb
            .insert(Location::Cursor(), &completion.accept())?;

        Ok(())
    }

    fn history_up(&mut self, state: &mut LineStateBundle) -> anyhow::Result<()> {
        // save current prompt
        if HistoryInd::Prompt == state.line.history_ind {
            state.line.saved_line = state.line.cb.slice(..).to_string();
        }

        state.line.history_ind = state.line.history_ind.up(state.ctx.history.len());
        self.update_history(state)?;

        Ok(())
    }

    fn history_down(&mut self, state: &mut LineStateBundle) -> anyhow::Result<()> {
        state.line.history_ind = state.line.history_ind.down();
        self.update_history(state)?;

        Ok(())
    }

    fn update_history(&mut self, state: &mut LineStateBundle) -> anyhow::Result<()> {
        match state.line.history_ind {
            // restore saved line
            HistoryInd::Prompt => {
                state.line.cb.clear();
                state
                    .line
                    .cb
                    .insert(Location::Cursor(), &state.line.saved_line)?;
            },
            // fill prompt with history element
            HistoryInd::Line(i) => {
                let history_item = state.ctx.history.get(i).unwrap();
                state.line.cb.clear();
                state.line.cb.insert(Location::Cursor(), history_item)?;
            },
        }
        Ok(())
    }

    fn to_normal_mode(&self, state: &mut LineStateBundle) -> anyhow::Result<()> {
        if let Some(cursor_style) = state.ctx.state.get_mut::<CursorStyle>() {
            cursor_style.style = SetCursorStyle::BlinkingBlock;
        }

        state.line.mode = LineMode::Normal;

        let hook_ctx = LineModeSwitchCtx {
            line_mode: LineMode::Normal,
        };
        state
            .sh
            .hooks
            .run::<LineModeSwitchCtx>(state.sh, state.ctx, state.rt, hook_ctx)?;
        Ok(())
    }

    fn to_insert_mode(&self, state: &mut LineStateBundle) -> anyhow::Result<()> {
        if let Some(cursor_style) = state.ctx.state.get_mut::<CursorStyle>() {
            cursor_style.style = SetCursorStyle::BlinkingBar;
        }

        state.line.mode = LineMode::Insert;

        let hook_ctx = LineModeSwitchCtx {
            line_mode: LineMode::Insert,
        };
        state
            .sh
            .hooks
            .run::<LineModeSwitchCtx>(state.sh, state.ctx, state.rt, hook_ctx)?;
        Ok(())
    }
}
