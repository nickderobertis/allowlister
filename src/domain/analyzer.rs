//! Decompose a bash command into role-tagged [`Fragment`]s.
//!
//! The whole point of the engine is that rules never see pipelines or
//! subshells — only `(argv, role, redirections)` tuples. The bash AST is
//! walked once; every `SimpleCommand` becomes one [`Fragment`] annotated with
//! the structural role it plays. Composition (pipes, `&&`, substitutions) is
//! captured entirely by the role tag plus a flat fragment list.

use brush_parser::ast;
use brush_parser::word::{self, WordPiece, WordPieceWithSource};
use brush_parser::{Parser, ParserOptions};

/// Bound on nesting/substitution recursion. Real commands never approach this;
/// the limit only prevents pathological or adversarial inputs from looping.
const MAX_DEPTH: u32 = 64;

/// Commands that wrap another command but do not change its safety profile.
/// They are stripped before a fragment's argv is emitted so a rule for
/// `npm test` still matches `timeout 30 npm test`. This mirrors the harness's
/// own pre-match normalization.
const PROCESS_WRAPPERS: &[&str] = &["timeout", "time", "nice", "nohup", "stdbuf"];

/// The structural role a command plays within its shell expression.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    /// Top-level command whose output goes to the terminal.
    Standalone,
    /// Leftmost command in a pipeline (its output is piped onward).
    PipeSource,
    /// Non-leftmost command in a pipeline (stdin comes from another fragment).
    PipeFilter,
    /// Inside `( … )`, `{ …; }`, or a `for`/`while`/`until`/`if` body.
    Subshell,
    /// Inside `$(…)`, backticks, or `<(…)`/`>(…)` process substitution.
    Substitution,
}

impl Role {
    /// The wire/config string for this role.
    pub fn as_str(self) -> &'static str {
        match self {
            Role::Standalone => "standalone",
            Role::PipeSource => "pipe_source",
            Role::PipeFilter => "pipe_filter",
            Role::Subshell => "subshell",
            Role::Substitution => "substitution",
        }
    }

    /// Parse a role from its config string.
    pub fn parse(value: &str) -> Option<Role> {
        match value {
            "standalone" => Some(Role::Standalone),
            "pipe_source" => Some(Role::PipeSource),
            "pipe_filter" => Some(Role::PipeFilter),
            "subshell" => Some(Role::Subshell),
            "substitution" => Some(Role::Substitution),
            _ => None,
        }
    }
}

/// How a redirection is treated by the rule engine.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RedirClass {
    /// Writes to a file target (`>`, `>>`, `>|`, `<>`, `&>`).
    Write,
    /// Reads from a file or data target (`<`, `<<`, `<<<`).
    Read,
    /// File-descriptor manipulation with no file target (`2>&1`, `>&3`).
    Neutral,
}

/// A single redirection attached to a fragment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Redirection {
    pub class: RedirClass,
    /// The resolved file target, when the redirection names one. `None` for
    /// fd duplications, here-documents, and here-strings (data, not files).
    pub target: Option<String>,
    /// Human-readable form for diagnostics, e.g. `> /tmp/x.txt`.
    pub display: String,
}

/// One `SimpleCommand` observed in the input, tagged with its role.
#[derive(Clone, Debug)]
pub struct Fragment {
    pub argv: Vec<String>,
    pub role: Role,
    pub redirections: Vec<Redirection>,
}

impl Fragment {
    /// argv joined by single spaces (the form matched by `match` rules).
    pub fn cmd_string(&self) -> String {
        self.argv.join(" ")
    }
}

/// Result of analyzing a command string.
#[derive(Clone, Debug, Default)]
pub struct Analysis {
    pub fragments: Vec<Fragment>,
    /// Non-fatal notes (suppressed dynamic command names, nested parse errors).
    pub warnings: Vec<String>,
    /// Constructs we intentionally do not analyze (e.g. function definitions).
    pub unsupported: Vec<String>,
}

/// Parse `source` and produce a flat fragment list with diagnostics.
pub fn analyze(source: &str) -> Analysis {
    let options = ParserOptions::default();
    let mut analysis = Analysis::default();
    match parse_program(source, &options) {
        Ok(program) => {
            let mut walker = Walker {
                options: &options,
                out: &mut analysis,
                depth: 0,
            };
            for command in &program.complete_commands {
                walker.walk_compound_list(command, Role::Standalone);
            }
        }
        Err(err) => analysis.warnings.push(format!("parse error: {err}")),
    }
    analysis
}

fn parse_program(source: &str, options: &ParserOptions) -> Result<ast::Program, String> {
    let reader = std::io::Cursor::new(source.as_bytes().to_vec());
    let mut parser = Parser::new(reader, options);
    parser.parse_program().map_err(|e| e.to_string())
}

struct Walker<'a> {
    options: &'a ParserOptions,
    out: &'a mut Analysis,
    depth: u32,
}

impl Walker<'_> {
    /// The role for commands nested inside a compound construct: a substitution
    /// stays a substitution; anything else becomes a subshell.
    fn nested_role(role: Role) -> Role {
        match role {
            Role::Substitution => Role::Substitution,
            _ => Role::Subshell,
        }
    }

    fn walk_compound_list(&mut self, list: &ast::CompoundList, role: Role) {
        for item in &list.0 {
            // item.1 is the `;`/`&` separator and does not change the role.
            self.walk_and_or_list(&item.0, role);
        }
    }

    fn walk_and_or_list(&mut self, list: &ast::AndOrList, role: Role) {
        self.walk_pipeline(&list.first, role);
        for and_or in &list.additional {
            let pipeline = match and_or {
                ast::AndOr::And(p) | ast::AndOr::Or(p) => p,
            };
            self.walk_pipeline(pipeline, role);
        }
    }

    fn walk_pipeline(&mut self, pipeline: &ast::Pipeline, role: Role) {
        match pipeline.seq.as_slice() {
            [] => {}
            [single] => self.walk_command(single, role),
            many => {
                for (index, command) in many.iter().enumerate() {
                    let piped_role = if index == 0 {
                        Role::PipeSource
                    } else {
                        Role::PipeFilter
                    };
                    self.walk_command(command, piped_role);
                }
            }
        }
    }

    fn walk_command(&mut self, command: &ast::Command, role: Role) {
        match command {
            ast::Command::Simple(simple) => self.build_fragment(simple, role),
            ast::Command::Compound(compound, redirects) => {
                self.walk_compound_command(compound, role);
                if let Some(list) = redirects {
                    self.note_group_redirects(list);
                }
            }
            ast::Command::Function(_) => self
                .out
                .unsupported
                .push("function definition not supported; manual review required".to_string()),
            // `[[ … ]]` is a side-effect-free shell builtin conditional; it
            // executes no external command, so there is nothing to gate.
            ast::Command::ExtendedTest(_, _) => {}
        }
    }

    fn walk_compound_command(&mut self, compound: &ast::CompoundCommand, role: Role) {
        let inner = Self::nested_role(role);
        match compound {
            ast::CompoundCommand::Subshell(s) => self.walk_compound_list(&s.list, inner),
            ast::CompoundCommand::BraceGroup(b) => self.walk_compound_list(&b.list, inner),
            ast::CompoundCommand::ForClause(f) => self.walk_compound_list(&f.body.list, inner),
            ast::CompoundCommand::ArithmeticForClause(f) => {
                self.walk_compound_list(&f.body.list, inner)
            }
            ast::CompoundCommand::WhileClause(w) | ast::CompoundCommand::UntilClause(w) => {
                self.walk_compound_list(&w.0, inner);
                self.walk_compound_list(&w.1.list, inner);
            }
            ast::CompoundCommand::IfClause(i) => {
                self.walk_compound_list(&i.condition, inner);
                self.walk_compound_list(&i.then, inner);
                if let Some(elses) = &i.elses {
                    for clause in elses {
                        if let Some(cond) = &clause.condition {
                            self.walk_compound_list(cond, inner);
                        }
                        self.walk_compound_list(&clause.body, inner);
                    }
                }
            }
            ast::CompoundCommand::CaseClause(c) => {
                for case in &c.cases {
                    if let Some(cmd) = &case.cmd {
                        self.walk_compound_list(cmd, inner);
                    }
                }
            }
            ast::CompoundCommand::Coprocess(c) => self.walk_command(&c.body, inner),
            // `(( … ))` evaluates arithmetic only; no command runs.
            ast::CompoundCommand::Arithmetic(_) => {}
        }
    }

    fn build_fragment(&mut self, simple: &ast::SimpleCommand, role: Role) {
        let mut argv: Vec<String> = Vec::new();
        let mut redirections: Vec<Redirection> = Vec::new();
        let mut inner_subs: Vec<String> = Vec::new();
        let mut command_name_is_substitution = false;

        if let Some(prefix) = &simple.prefix {
            for item in &prefix.0 {
                self.handle_item(item, true, &mut argv, &mut redirections, &mut inner_subs);
            }
        }

        if let Some(name) = &simple.word_or_name {
            let (subs, whole_is_sub) = self.analyze_word(&name.value);
            inner_subs.extend(subs);
            command_name_is_substitution = whole_is_sub;
            argv.push(name.value.clone());
        }

        if let Some(suffix) = &simple.suffix {
            for item in &suffix.0 {
                self.handle_item(item, false, &mut argv, &mut redirections, &mut inner_subs);
            }
        }

        // Inner command substitutions run regardless of the outer fragment's
        // fate, so evaluate them before any suppression.
        for source in inner_subs {
            self.walk_source(&source, Role::Substitution);
        }

        if argv.is_empty() {
            return; // bare assignment(s) or redirect-only — nothing to gate.
        }

        if command_name_is_substitution {
            self.out.warnings.push(format!(
                "command name `{}` is a substitution; outer fragment suppressed \
                 (inner substitution evaluated separately)",
                argv[0]
            ));
            return;
        }

        let argv = strip_wrappers(argv);
        if argv.is_empty() {
            return; // a wrapper with no wrapped command.
        }

        self.out.fragments.push(Fragment {
            argv,
            role,
            redirections,
        });
    }

    /// Handle one prefix/suffix item. `in_prefix` distinguishes a leading
    /// `FOO=bar` (an environment assignment, ignored for matching) from a
    /// trailing `FOO=bar` (an ordinary argument in bash).
    fn handle_item(
        &mut self,
        item: &ast::CommandPrefixOrSuffixItem,
        in_prefix: bool,
        argv: &mut Vec<String>,
        redirections: &mut Vec<Redirection>,
        inner_subs: &mut Vec<String>,
    ) {
        match item {
            ast::CommandPrefixOrSuffixItem::AssignmentWord(_, _) if in_prefix => {
                // Environment assignment prefixing the command — recorded but
                // not part of the command we gate.
            }
            ast::CommandPrefixOrSuffixItem::Word(word)
            | ast::CommandPrefixOrSuffixItem::AssignmentWord(_, word) => {
                let (subs, _) = self.analyze_word(&word.value);
                inner_subs.extend(subs);
                argv.push(word.value.clone());
            }
            ast::CommandPrefixOrSuffixItem::IoRedirect(redirect) => {
                self.collect_redirect(redirect, redirections);
            }
            ast::CommandPrefixOrSuffixItem::ProcessSubstitution(_, subshell) => {
                self.walk_compound_list(&subshell.list, Role::Substitution);
            }
        }
    }

    fn collect_redirect(&mut self, redirect: &ast::IoRedirect, out: &mut Vec<Redirection>) {
        match redirect {
            ast::IoRedirect::File(_, kind, target) => {
                let (class, op) = redirect_kind(kind);
                match target {
                    ast::IoFileRedirectTarget::Filename(word) => out.push(Redirection {
                        class,
                        target: Some(word.value.clone()),
                        display: format!("{op} {}", word.value),
                    }),
                    ast::IoFileRedirectTarget::Fd(fd) => out.push(Redirection {
                        class: RedirClass::Neutral,
                        target: None,
                        display: format!("{op}{fd}"),
                    }),
                    ast::IoFileRedirectTarget::Duplicate(word) => out.push(Redirection {
                        class: RedirClass::Neutral,
                        target: None,
                        display: format!("{op}{}", word.value),
                    }),
                    ast::IoFileRedirectTarget::ProcessSubstitution(_, subshell) => {
                        self.walk_compound_list(&subshell.list, Role::Substitution);
                        out.push(Redirection {
                            class: RedirClass::Neutral,
                            target: None,
                            display: format!("{op} <(…)"),
                        });
                    }
                }
            }
            ast::IoRedirect::OutputAndError(word, append) => {
                let op = if *append { "&>>" } else { "&>" };
                out.push(Redirection {
                    class: RedirClass::Write,
                    target: Some(word.value.clone()),
                    display: format!("{op} {}", word.value),
                });
            }
            // Here-document/here-string bodies are data, never code. They are
            // treated as reads with no file target.
            ast::IoRedirect::HereDocument(_, _) => out.push(Redirection {
                class: RedirClass::Read,
                target: None,
                display: "<<(here-document)".to_string(),
            }),
            ast::IoRedirect::HereString(_, word) => out.push(Redirection {
                class: RedirClass::Read,
                target: None,
                display: format!("<<< {}", word.value),
            }),
        }
    }

    fn note_group_redirects(&mut self, list: &ast::RedirectList) {
        // A redirect on a compound (e.g. `( … ) > out`) has no host fragment to
        // attach to. Surface it so a rule author is aware it is not gated.
        for redirect in &list.0 {
            let mut sink = Vec::new();
            self.collect_redirect(redirect, &mut sink);
            for redirection in sink {
                self.out.warnings.push(format!(
                    "redirection `{}` on a command group is not gated by per-command rules",
                    redirection.display
                ));
            }
        }
    }

    /// Re-parse and walk an inner command-substitution string.
    fn walk_source(&mut self, source: &str, role: Role) {
        if self.depth >= MAX_DEPTH {
            self.out
                .warnings
                .push("recursion limit reached; nested substitution not analyzed".to_string());
            return;
        }
        self.depth += 1;
        match parse_program(source, self.options) {
            Ok(program) => {
                for command in &program.complete_commands {
                    self.walk_compound_list(command, role);
                }
            }
            Err(err) => self
                .out
                .warnings
                .push(format!("parse error in substitution `{source}`: {err}")),
        }
        self.depth -= 1;
    }

    /// Inspect a word for command substitutions. Returns the inner command
    /// strings to evaluate, plus whether the entire word is a single
    /// substitution (a dynamic command name when this is argv[0]).
    fn analyze_word(&self, value: &str) -> (Vec<String>, bool) {
        if !value.contains('$') && !value.contains('`') {
            return (Vec::new(), false);
        }
        match word::parse(value, self.options) {
            Ok(pieces) => {
                let mut subs = Vec::new();
                collect_substitutions(&pieces, &mut subs);
                (subs, word_is_single_substitution(&pieces))
            }
            Err(_) => (Vec::new(), false),
        }
    }
}

fn redirect_kind(kind: &ast::IoFileRedirectKind) -> (RedirClass, &'static str) {
    match kind {
        ast::IoFileRedirectKind::Read => (RedirClass::Read, "<"),
        ast::IoFileRedirectKind::Write => (RedirClass::Write, ">"),
        ast::IoFileRedirectKind::Append => (RedirClass::Write, ">>"),
        ast::IoFileRedirectKind::Clobber => (RedirClass::Write, ">|"),
        ast::IoFileRedirectKind::ReadAndWrite => (RedirClass::Write, "<>"),
        ast::IoFileRedirectKind::DuplicateInput => (RedirClass::Neutral, "<&"),
        ast::IoFileRedirectKind::DuplicateOutput => (RedirClass::Neutral, ">&"),
    }
}

fn collect_substitutions(pieces: &[WordPieceWithSource], out: &mut Vec<String>) {
    for piece in pieces {
        match &piece.piece {
            WordPiece::CommandSubstitution(s) | WordPiece::BackquotedCommandSubstitution(s) => {
                out.push(s.clone());
            }
            WordPiece::DoubleQuotedSequence(inner)
            | WordPiece::GettextDoubleQuotedSequence(inner) => {
                collect_substitutions(inner, out);
            }
            _ => {}
        }
    }
}

fn word_is_single_substitution(pieces: &[WordPieceWithSource]) -> bool {
    match pieces {
        [only] => match &only.piece {
            WordPiece::CommandSubstitution(_) | WordPiece::BackquotedCommandSubstitution(_) => true,
            WordPiece::DoubleQuotedSequence(inner)
            | WordPiece::GettextDoubleQuotedSequence(inner) => word_is_single_substitution(inner),
            _ => false,
        },
        _ => false,
    }
}

/// Repeatedly strip leading process wrappers and bare `xargs` so rules match
/// the command that actually runs.
fn strip_wrappers(mut argv: Vec<String>) -> Vec<String> {
    while let Some(consumed) = wrapper_consume(&argv) {
        if consumed == 0 || consumed > argv.len() {
            break;
        }
        argv.drain(0..consumed);
    }
    argv
}

/// Number of leading tokens to drop if argv starts with a strippable wrapper.
fn wrapper_consume(argv: &[String]) -> Option<usize> {
    let head = argv.first()?.as_str();
    match head {
        "nohup" => Some(1),
        "time" => Some(1 + usize::from(argv.get(1).is_some_and(|a| a == "-p"))),
        "nice" => Some(1 + consume_options(&argv[1..], &["-n"])),
        "stdbuf" => Some(1 + consume_options(&argv[1..], &["-i", "-o", "-e"])),
        "timeout" => {
            let opts = consume_options(&argv[1..], &["-s", "-k", "--signal", "--kill-after"]);
            let after_opts = 1 + opts;
            // `timeout [opts] DURATION command …` — drop the duration too.
            Some(if argv.len() > after_opts {
                after_opts + 1
            } else {
                after_opts
            })
        }
        "xargs" => match argv.get(1) {
            // Bare `xargs command …` is stripped to mirror harness behavior;
            // flagged `xargs -nN command …` keeps `xargs` as the gated command.
            Some(next) if !next.starts_with('-') => Some(1),
            _ => None,
        },
        _ if PROCESS_WRAPPERS.contains(&head) => Some(1),
        _ => None,
    }
}

/// Consume leading option tokens (and the values of value-taking options).
/// Returns the number of tokens consumed.
fn consume_options(rest: &[String], value_options: &[&str]) -> usize {
    let mut i = 0;
    while i < rest.len() {
        let token = rest[i].as_str();
        if token == "--" {
            i += 1;
            break;
        }
        if !token.starts_with('-') {
            break;
        }
        if let Some(stripped) = token.strip_prefix("--") {
            if stripped.contains('=') {
                i += 1;
            } else if value_options.contains(&token) {
                i += 2; // separate value
            } else {
                i += 1;
            }
            continue;
        }
        let short = &token[..token.len().min(2)];
        if value_options.contains(&short) {
            // `-n10` carries its value; `-n 10` does not.
            i += if token.len() > 2 { 1 } else { 2 };
        } else {
            i += 1;
        }
    }
    i
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argvs(source: &str) -> Vec<(Vec<String>, Role)> {
        analyze(source)
            .fragments
            .into_iter()
            .map(|f| (f.argv, f.role))
            .collect()
    }

    #[test]
    fn pipeline_roles() {
        let frags = argvs("gh pr list | head -20");
        assert_eq!(frags.len(), 2);
        assert_eq!(frags[0].1, Role::PipeSource);
        assert_eq!(frags[1].1, Role::PipeFilter);
    }

    #[test]
    fn and_or_keeps_standalone() {
        let frags = argvs("gh pr list && git status");
        assert_eq!(frags.len(), 2);
        assert!(frags.iter().all(|(_, role)| *role == Role::Standalone));
    }

    #[test]
    fn substitution_inner_is_evaluated() {
        let frags = argvs("echo $(git rev-parse HEAD)");
        assert_eq!(frags.len(), 2);
        // outer echo is standalone, inner git rev-parse is a substitution.
        assert!(frags
            .iter()
            .any(|(argv, role)| argv[0] == "git" && *role == Role::Substitution));
    }

    #[test]
    fn pure_substitution_command_name_is_suppressed() {
        let analysis = analyze("$(some_unknown_cmd)");
        // outer suppressed; only the inner command remains.
        assert_eq!(analysis.fragments.len(), 1);
        assert_eq!(analysis.fragments[0].argv, vec!["some_unknown_cmd"]);
        assert_eq!(analysis.fragments[0].role, Role::Substitution);
        assert!(!analysis.warnings.is_empty());
    }

    #[test]
    fn function_definition_is_unsupported() {
        let analysis = analyze("f() { rm -rf /; }; f");
        assert!(!analysis.unsupported.is_empty());
    }

    #[test]
    fn process_wrappers_are_stripped() {
        assert_eq!(argvs("timeout 30 npm test")[0].0, vec!["npm", "test"]);
        assert_eq!(argvs("nice -n 10 git status")[0].0, vec!["git", "status"]);
        assert_eq!(argvs("nohup make")[0].0, vec!["make"]);
        assert_eq!(argvs("stdbuf -oL grep x")[0].0, vec!["grep", "x"]);
    }

    #[test]
    fn bare_xargs_stripped_flagged_xargs_kept() {
        assert_eq!(argvs("xargs grep TODO")[0].0, vec!["grep", "TODO"]);
        assert_eq!(
            argvs("xargs -n1 grep TODO")[0].0,
            vec!["xargs", "-n1", "grep", "TODO"]
        );
    }

    #[test]
    fn subshell_branches_get_subshell_role() {
        let frags = argvs("true && (git status; git diff)");
        let subshell: Vec<_> = frags
            .iter()
            .filter(|(_, role)| *role == Role::Subshell)
            .collect();
        assert_eq!(subshell.len(), 2);
    }

    #[test]
    fn redirection_is_captured() {
        let analysis = analyze("echo hi > /tmp/x.txt");
        let redir = &analysis.fragments[0].redirections[0];
        assert_eq!(redir.class, RedirClass::Write);
        assert_eq!(redir.target.as_deref(), Some("/tmp/x.txt"));
    }

    #[test]
    fn process_substitution_inner_evaluated() {
        let analysis = analyze("diff <(git show HEAD:a) <(git show HEAD~1:a)");
        let subs = analysis
            .fragments
            .iter()
            .filter(|f| f.role == Role::Substitution)
            .count();
        assert_eq!(subs, 2);
    }

    #[test]
    fn parse_error_yields_warning_not_panic() {
        let analysis = analyze("for do done (");
        // Must not panic; either warns or produces no fragments.
        assert!(analysis.fragments.is_empty() || !analysis.warnings.is_empty());
    }

    #[test]
    fn for_loop_body_is_subshell() {
        let frags = argvs("for f in a b; do echo $f; done");
        assert_eq!(frags.len(), 1);
        assert_eq!(frags[0].0[0], "echo");
        assert_eq!(frags[0].1, Role::Subshell);
    }

    #[test]
    fn while_loop_condition_and_body_are_walked() {
        let frags = argvs("while true; do git status; done");
        // condition `true` and body `git status` both become subshell fragments.
        assert_eq!(frags.len(), 2);
        assert!(frags.iter().all(|(_, role)| *role == Role::Subshell));
    }

    #[test]
    fn until_loop_is_walked() {
        let frags = argvs("until false; do echo wait; done");
        assert!(frags.iter().any(|(argv, _)| argv[0] == "echo"));
    }

    #[test]
    fn case_branches_are_subshell() {
        let frags = argvs("case $x in a) echo one;; *) echo other;; esac");
        assert_eq!(frags.len(), 2);
        assert!(frags.iter().all(|(_, role)| *role == Role::Subshell));
    }

    #[test]
    fn brace_group_is_subshell() {
        let frags = argvs("{ git status; git diff; }");
        assert_eq!(frags.len(), 2);
        assert!(frags.iter().all(|(_, role)| *role == Role::Subshell));
    }

    #[test]
    fn append_and_clobber_are_write_redirects() {
        let append = analyze("echo x >> /tmp/log").fragments.pop().unwrap();
        assert_eq!(append.redirections[0].class, RedirClass::Write);
        let clobber = analyze("echo x >| /tmp/log").fragments.pop().unwrap();
        assert_eq!(clobber.redirections[0].class, RedirClass::Write);
    }

    #[test]
    fn read_redirect_has_read_class() {
        let frag = analyze("wc -l < /etc/hosts").fragments.pop().unwrap();
        assert_eq!(frag.redirections[0].class, RedirClass::Read);
        assert_eq!(frag.redirections[0].target.as_deref(), Some("/etc/hosts"));
    }

    #[test]
    fn fd_duplication_is_neutral() {
        let frag = analyze("make 2>&1").fragments.pop().unwrap();
        assert!(frag
            .redirections
            .iter()
            .any(|r| r.class == RedirClass::Neutral));
    }

    #[test]
    fn output_and_error_redirect_is_write() {
        let frag = analyze("make &> /tmp/out").fragments.pop().unwrap();
        assert!(frag
            .redirections
            .iter()
            .any(|r| r.class == RedirClass::Write && r.target.as_deref() == Some("/tmp/out")));
    }

    #[test]
    fn here_string_and_here_doc_are_read_data() {
        let here_string = analyze("cat <<< hello").fragments.pop().unwrap();
        assert_eq!(here_string.redirections[0].class, RedirClass::Read);
        assert!(here_string.redirections[0].target.is_none());

        let here_doc = analyze("cat <<EOF\nbody\nEOF").fragments.pop().unwrap();
        assert!(here_doc
            .redirections
            .iter()
            .any(|r| r.class == RedirClass::Read));
    }

    #[test]
    fn assignment_prefix_is_ignored_in_argv() {
        let frag = analyze("FOO=bar git status").fragments.pop().unwrap();
        assert_eq!(frag.argv, vec!["git", "status"]);
    }

    #[test]
    fn group_redirect_is_reported_as_warning() {
        let analysis = analyze("(echo a; echo b) > /tmp/out");
        assert!(analysis.warnings.iter().any(|w| w.contains("group")));
    }

    #[test]
    fn time_wrapper_with_posix_flag_is_handled() {
        // `time` is a pipeline keyword in bash; the wrapped command is gated.
        let frags = argvs("time git status");
        assert!(frags.iter().any(|(argv, _)| argv[0] == "git"));
    }

    #[test]
    fn long_option_wrappers_are_stripped() {
        // `--signal=TERM` (attached value) then the duration `5`, then command.
        assert_eq!(
            argvs("timeout --signal=TERM 5 npm test")[0].0,
            vec!["npm", "test"]
        );
        // `-k 5` (kill-after, separate value) then the duration `10`.
        assert_eq!(
            argvs("timeout -k 5 10 git status")[0].0,
            vec!["git", "status"]
        );
    }

    #[test]
    fn double_quoted_substitution_is_collected() {
        let frags = argvs(r#"echo "result: $(git rev-parse HEAD)""#);
        assert!(frags
            .iter()
            .any(|(argv, role)| argv[0] == "git" && *role == Role::Substitution));
    }

    #[test]
    fn if_elif_else_branches_are_walked() {
        let frags = argvs("if true; then git status; elif false; then git diff; else echo x; fi");
        let names: Vec<&str> = frags.iter().map(|(a, _)| a[0].as_str()).collect();
        assert!(names.contains(&"git"));
        assert!(names.contains(&"echo"));
        assert!(frags.iter().all(|(_, role)| *role == Role::Subshell));
    }

    #[test]
    fn arithmetic_for_loop_body_is_walked() {
        let frags = argvs("for ((i = 0; i < 2; i++)); do echo $i; done");
        assert!(frags
            .iter()
            .any(|(argv, role)| argv[0] == "echo" && *role == Role::Subshell));
    }

    #[test]
    fn extended_test_runs_no_command() {
        // `[[ … ]]` is a builtin conditional; on its own it gates nothing.
        assert!(analyze("[[ -f /etc/hosts ]]").fragments.is_empty());
        let frags = argvs("[[ -f x ]] && echo ok");
        assert!(frags.iter().any(|(argv, _)| argv[0] == "echo"));
    }

    #[test]
    fn arithmetic_command_runs_nothing() {
        assert!(analyze("(( count++ ))").fragments.is_empty());
    }

    #[test]
    fn coprocess_does_not_panic() {
        // Behavior varies by parser; only the no-panic contract is asserted.
        let _ = analyze("coproc git log --oneline");
    }

    #[test]
    fn process_substitution_as_redirect_target_is_walked() {
        let analysis = analyze("wc -l < <(git log --oneline)");
        assert!(analysis.fragments.iter().any(|f| f.argv[0] == "wc"));
    }

    #[test]
    fn duplicate_input_fd_is_neutral() {
        let frag = analyze("cat <&3").fragments.pop().unwrap();
        assert!(frag
            .redirections
            .iter()
            .any(|r| r.class == RedirClass::Neutral));
    }

    #[test]
    fn read_write_redirect_is_captured() {
        let frag = analyze("cat <> /tmp/rw").fragments.pop().unwrap();
        assert!(!frag.redirections.is_empty());
    }

    #[test]
    fn backquoted_substitution_is_evaluated() {
        let frags = argvs("echo `git rev-parse HEAD`");
        assert!(frags
            .iter()
            .any(|(argv, role)| argv[0] == "git" && *role == Role::Substitution));
    }

    #[test]
    fn pure_backquoted_command_name_is_suppressed() {
        let analysis = analyze("`some_unknown_cmd`");
        assert!(analysis
            .fragments
            .iter()
            .any(|f| f.argv[0] == "some_unknown_cmd" && f.role == Role::Substitution));
        assert!(!analysis.warnings.is_empty());
    }

    #[test]
    fn substitution_parse_error_warns() {
        let analysis = analyze("echo $(for x)");
        assert!(analysis.warnings.iter().any(|w| w.contains("parse error")));
    }

    #[test]
    fn deep_substitution_nesting_hits_recursion_limit() {
        let mut src = String::from("echo hi");
        for _ in 0..70 {
            src = format!("echo $({src})");
        }
        let analysis = analyze(&src);
        assert!(analysis
            .warnings
            .iter()
            .any(|w| w.contains("recursion limit")));
    }

    #[test]
    fn wrapper_option_double_dash_terminates() {
        // `--` ends the wrapper's own options; the command after it is gated.
        let frags = argvs("nice -n 5 -- git status");
        assert!(frags.iter().any(|(argv, _)| argv[0] == "git"));
    }
}
