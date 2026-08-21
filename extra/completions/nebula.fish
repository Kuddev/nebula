# Print an optspec for argparse to handle cmd's options that are independent of any subcommand.
function __fish_nebula_global_optspecs
	string join \n print-events ref-test embed= gpui legacy-shell config-file= q v daemon working-directory= hold e/command= T/title= class= o/option= h/help V/version
end

function __fish_nebula_needs_command
	# Figure out if the current invocation already has a command.
	set -l cmd (commandline -opc)
	set -e cmd[1]
	argparse -s (__fish_nebula_global_optspecs) -- $cmd 2>/dev/null
	or return
	if set -q argv[1]
		# Also print the command, so this can be used to figure out what it is.
		echo $argv[1]
		return 1
	end
	return 0
end

function __fish_nebula_using_subcommand
	set -l cmd (__fish_nebula_needs_command)
	test -z "$cmd"
	and return 1
	contains -- $cmd[1] $argv
end

complete -c nebula -n "__fish_nebula_needs_command" -l embed -d 'X11 window ID to embed Nebula within (decimal or hexadecimal with "0x" prefix)' -r
complete -c nebula -n "__fish_nebula_needs_command" -l config-file -d 'Specify alternative configuration file [default: %APPDATA%\\nebula\\nebula.toml]' -r -F
complete -c nebula -n "__fish_nebula_needs_command" -l working-directory -d 'Start the shell in the specified working directory' -r -F
complete -c nebula -n "__fish_nebula_needs_command" -s e -l command -d 'Command and args to execute (must be last argument)' -r
complete -c nebula -n "__fish_nebula_needs_command" -s T -l title -d 'Defines the window title [default: Nebula Terminal]' -r
complete -c nebula -n "__fish_nebula_needs_command" -l class -d 'Defines window class/app_id on X11/Wayland [default: Nebula]' -r
complete -c nebula -n "__fish_nebula_needs_command" -s o -l option -d 'Override configuration file options [example: \'cursor.style="Beam"\']' -r
complete -c nebula -n "__fish_nebula_needs_command" -l print-events -d 'Print all events to STDOUT'
complete -c nebula -n "__fish_nebula_needs_command" -l ref-test -d 'Generates ref test'
complete -c nebula -n "__fish_nebula_needs_command" -l gpui -d 'Launch the GPUI UI shell as the main window'
complete -c nebula -n "__fish_nebula_needs_command" -l legacy-shell -d 'Launch the legacy winit shell instead of GPUI'
complete -c nebula -n "__fish_nebula_needs_command" -s q -d 'Reduces the level of verbosity (the min level is -qq)'
complete -c nebula -n "__fish_nebula_needs_command" -s v -d 'Increases the level of verbosity (the max level is -vvv)'
complete -c nebula -n "__fish_nebula_needs_command" -l daemon -d 'Do not spawn an initial window'
complete -c nebula -n "__fish_nebula_needs_command" -l hold -d 'Remain open after child process exit'
complete -c nebula -n "__fish_nebula_needs_command" -s h -l help -d 'Print help (see more with \'--help\')'
complete -c nebula -n "__fish_nebula_needs_command" -s V -l version -d 'Print version'
complete -c nebula -n "__fish_nebula_needs_command" -f -a "ctl" -d 'Agent-oriented terminal control: split panes, run commands, start Codex/Claude, send prompts, wait for state changes, and read verified terminal output'
complete -c nebula -n "__fish_nebula_needs_command" -f -a "migrate" -d 'Migrate the configuration file'
complete -c nebula -n "__fish_nebula_needs_command" -f -a "config" -d 'Validate or create the Nebula configuration'
complete -c nebula -n "__fish_nebula_needs_command" -f -a "notify-test" -d 'Test system notification (toast) delivery'
complete -c nebula -n "__fish_nebula_needs_command" -f -a "setup-ai" -d 'Install (or --remove) AI hooks plus the Nebula Runtime Skill for Codex and Claude Code'
complete -c nebula -n "__fish_nebula_needs_command" -f -a "ssh" -d 'SSH with Nebula shell integration bootstrapped on the remote host, so tab icons / spinner / cwd track the program running over the connection (claude, vim, cargo…). All arguments are forwarded to the system `ssh`'
complete -c nebula -n "__fish_nebula_needs_command" -f -a "help" -d 'Print this message or the help of the given subcommand(s)'
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and not __fish_seen_subcommand_from describe snapshot orchestrate agents agent-start agent-fork agent-get agent-prompt agent-read agent-wait subscribe new-window focus new-tab split prompt read procs send-key run wait help" -l timeout-ms -d 'Maximum time to wait for a command response' -r
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and not __fish_seen_subcommand_from describe snapshot orchestrate agents agent-start agent-fork agent-get agent-prompt agent-read agent-wait subscribe new-window focus new-tab split prompt read procs send-key run wait help" -l pretty -d 'Pretty-print one-shot JSON responses. Streaming subscriptions stay JSON Lines'
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and not __fish_seen_subcommand_from describe snapshot orchestrate agents agent-start agent-fork agent-get agent-prompt agent-read agent-wait subscribe new-window focus new-tab split prompt read procs send-key run wait help" -s h -l help -d 'Print help'
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and not __fish_seen_subcommand_from describe snapshot orchestrate agents agent-start agent-fork agent-get agent-prompt agent-read agent-wait subscribe new-window focus new-tab split prompt read procs send-key run wait help" -f -a "describe" -d 'Describe the protocol version, runtime version, and available capabilities'
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and not __fish_seen_subcommand_from describe snapshot orchestrate agents agent-start agent-fork agent-get agent-prompt agent-read agent-wait subscribe new-window focus new-tab split prompt read procs send-key run wait help" -f -a "snapshot" -d 'Read the authoritative window, tab, pane, and task-state projection'
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and not __fish_seen_subcommand_from describe snapshot orchestrate agents agent-start agent-fork agent-get agent-prompt agent-read agent-wait subscribe new-window focus new-tab split prompt read procs send-key run wait help" -f -a "orchestrate" -d 'Execute one typed multi-step terminal workflow in a single Runtime request'
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and not __fish_seen_subcommand_from describe snapshot orchestrate agents agent-start agent-fork agent-get agent-prompt agent-read agent-wait subscribe new-window focus new-tab split prompt read procs send-key run wait help" -f -a "agents" -d 'List only panes Nebula recognizes as AI agents, with semantic state and session identity'
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and not __fish_seen_subcommand_from describe snapshot orchestrate agents agent-start agent-fork agent-get agent-prompt agent-read agent-wait subscribe new-window focus new-tab split prompt read procs send-key run wait help" -f -a "agent-start" -d 'Start a verified AI CLI in a new terminal tab and assign a stable name'
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and not __fish_seen_subcommand_from describe snapshot orchestrate agents agent-start agent-fork agent-get agent-prompt agent-read agent-wait subscribe new-window focus new-tab split prompt read procs send-key run wait help" -f -a "agent-fork" -d 'Create an isolated Git worktree, then start a named AI CLI in that checkout'
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and not __fish_seen_subcommand_from describe snapshot orchestrate agents agent-start agent-fork agent-get agent-prompt agent-read agent-wait subscribe new-window focus new-tab split prompt read procs send-key run wait help" -f -a "agent-get" -d 'Resolve one managed agent by stable id or active name'
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and not __fish_seen_subcommand_from describe snapshot orchestrate agents agent-start agent-fork agent-get agent-prompt agent-read agent-wait subscribe new-window focus new-tab split prompt read procs send-key run wait help" -f -a "agent-prompt" -d 'Send one plain-text prompt to a managed agent'
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and not __fish_seen_subcommand_from describe snapshot orchestrate agents agent-start agent-fork agent-get agent-prompt agent-read agent-wait subscribe new-window focus new-tab split prompt read procs send-key run wait help" -f -a "agent-read" -d 'Read the terminal-buffer tail owned by a managed agent'
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and not __fish_seen_subcommand_from describe snapshot orchestrate agents agent-start agent-fork agent-get agent-prompt agent-read agent-wait subscribe new-window focus new-tab split prompt read procs send-key run wait help" -f -a "agent-wait" -d 'Wait for the same managed-agent generation to reach a semantic state'
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and not __fish_seen_subcommand_from describe snapshot orchestrate agents agent-start agent-fork agent-get agent-prompt agent-read agent-wait subscribe new-window focus new-tab split prompt read procs send-key run wait help" -f -a "subscribe" -d 'Stream state snapshots whenever their semantic content changes'
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and not __fish_seen_subcommand_from describe snapshot orchestrate agents agent-start agent-fork agent-get agent-prompt agent-read agent-wait subscribe new-window focus new-tab split prompt read procs send-key run wait help" -f -a "new-window" -d 'Create and focus a new terminal window'
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and not __fish_seen_subcommand_from describe snapshot orchestrate agents agent-start agent-fork agent-get agent-prompt agent-read agent-wait subscribe new-window focus new-tab split prompt read procs send-key run wait help" -f -a "focus" -d 'Focus a window or one of its panes'
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and not __fish_seen_subcommand_from describe snapshot orchestrate agents agent-start agent-fork agent-get agent-prompt agent-read agent-wait subscribe new-window focus new-tab split prompt read procs send-key run wait help" -f -a "new-tab" -d 'Create a default-shell tab in the target window'
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and not __fish_seen_subcommand_from describe snapshot orchestrate agents agent-start agent-fork agent-get agent-prompt agent-read agent-wait subscribe new-window focus new-tab split prompt read procs send-key run wait help" -f -a "split" -d 'Split the focused pane in the target window'
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and not __fish_seen_subcommand_from describe snapshot orchestrate agents agent-start agent-fork agent-get agent-prompt agent-read agent-wait subscribe new-window focus new-tab split prompt read procs send-key run wait help" -f -a "prompt" -d 'Send one plain-text prompt to a pane, optionally submitting it with Enter'
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and not __fish_seen_subcommand_from describe snapshot orchestrate agents agent-start agent-fork agent-get agent-prompt agent-read agent-wait subscribe new-window focus new-tab split prompt read procs send-key run wait help" -f -a "read" -d 'Read the latest logical lines from a pane\'s real terminal buffer'
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and not __fish_seen_subcommand_from describe snapshot orchestrate agents agent-start agent-fork agent-get agent-prompt agent-read agent-wait subscribe new-window focus new-tab split prompt read procs send-key run wait help" -f -a "procs" -d 'List the real local process tree rooted at a pane\'s PTY shell'
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and not __fish_seen_subcommand_from describe snapshot orchestrate agents agent-start agent-fork agent-get agent-prompt agent-read agent-wait subscribe new-window focus new-tab split prompt read procs send-key run wait help" -f -a "send-key" -d 'Send a restricted named control key using the pane\'s active terminal mode'
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and not __fish_seen_subcommand_from describe snapshot orchestrate agents agent-start agent-fork agent-get agent-prompt agent-read agent-wait subscribe new-window focus new-tab split prompt read procs send-key run wait help" -f -a "run" -d 'Run one shell command and return its real OSC 133 exit status'
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and not __fish_seen_subcommand_from describe snapshot orchestrate agents agent-start agent-fork agent-get agent-prompt agent-read agent-wait subscribe new-window focus new-tab split prompt read procs send-key run wait help" -f -a "wait" -d 'Wait until a pane reaches a semantic task state'
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and not __fish_seen_subcommand_from describe snapshot orchestrate agents agent-start agent-fork agent-get agent-prompt agent-read agent-wait subscribe new-window focus new-tab split prompt read procs send-key run wait help" -f -a "help" -d 'Print this message or the help of the given subcommand(s)'
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and __fish_seen_subcommand_from describe" -l timeout-ms -d 'Maximum time to wait for a command response' -r
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and __fish_seen_subcommand_from describe" -l pretty -d 'Pretty-print one-shot JSON responses. Streaming subscriptions stay JSON Lines'
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and __fish_seen_subcommand_from describe" -s h -l help -d 'Print help'
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and __fish_seen_subcommand_from snapshot" -l timeout-ms -d 'Maximum time to wait for a command response' -r
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and __fish_seen_subcommand_from snapshot" -l pretty -d 'Pretty-print one-shot JSON responses. Streaming subscriptions stay JSON Lines'
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and __fish_seen_subcommand_from snapshot" -s h -l help -d 'Print help'
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and __fish_seen_subcommand_from orchestrate" -l spec -d 'Inline UTF-8 JSON object containing steps and on_error' -r
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and __fish_seen_subcommand_from orchestrate" -l file -d 'Read the UTF-8 workflow JSON object from a file' -r -F
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and __fish_seen_subcommand_from orchestrate" -l timeout-ms -d 'Maximum time to wait for a command response' -r
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and __fish_seen_subcommand_from orchestrate" -l pretty -d 'Pretty-print one-shot JSON responses. Streaming subscriptions stay JSON Lines'
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and __fish_seen_subcommand_from orchestrate" -s h -l help -d 'Print help'
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and __fish_seen_subcommand_from agents" -l window -r
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and __fish_seen_subcommand_from agents" -l timeout-ms -d 'Maximum time to wait for a command response' -r
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and __fish_seen_subcommand_from agents" -l pretty -d 'Pretty-print one-shot JSON responses. Streaming subscriptions stay JSON Lines'
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and __fish_seen_subcommand_from agents" -s h -l help -d 'Print help'
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and __fish_seen_subcommand_from agent-start" -l window -r
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and __fish_seen_subcommand_from agent-start" -l name -r
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and __fish_seen_subcommand_from agent-start" -l kind -r
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and __fish_seen_subcommand_from agent-start" -l cwd -r -F
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and __fish_seen_subcommand_from agent-start" -l resume-session-id -r
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and __fish_seen_subcommand_from agent-start" -l timeout-ms -d 'Maximum time to wait for a command response' -r
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and __fish_seen_subcommand_from agent-start" -l pretty -d 'Pretty-print one-shot JSON responses. Streaming subscriptions stay JSON Lines'
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and __fish_seen_subcommand_from agent-start" -s h -l help -d 'Print help'
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and __fish_seen_subcommand_from agent-fork" -l window -r
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and __fish_seen_subcommand_from agent-fork" -l source-pane -r
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and __fish_seen_subcommand_from agent-fork" -l source-cwd -r -F
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and __fish_seen_subcommand_from agent-fork" -l name -r
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and __fish_seen_subcommand_from agent-fork" -l kind -r
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and __fish_seen_subcommand_from agent-fork" -l resume-session-id -r
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and __fish_seen_subcommand_from agent-fork" -l branch -r
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and __fish_seen_subcommand_from agent-fork" -l base -r
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and __fish_seen_subcommand_from agent-fork" -l path -r -F
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and __fish_seen_subcommand_from agent-fork" -l timeout-ms -d 'Maximum time to wait for a command response' -r
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and __fish_seen_subcommand_from agent-fork" -l allow-dirty-source
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and __fish_seen_subcommand_from agent-fork" -l pretty -d 'Pretty-print one-shot JSON responses. Streaming subscriptions stay JSON Lines'
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and __fish_seen_subcommand_from agent-fork" -s h -l help -d 'Print help'
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and __fish_seen_subcommand_from agent-get" -l agent -r
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and __fish_seen_subcommand_from agent-get" -l generation -r
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and __fish_seen_subcommand_from agent-get" -l timeout-ms -d 'Maximum time to wait for a command response' -r
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and __fish_seen_subcommand_from agent-get" -l pretty -d 'Pretty-print one-shot JSON responses. Streaming subscriptions stay JSON Lines'
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and __fish_seen_subcommand_from agent-get" -s h -l help -d 'Print help'
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and __fish_seen_subcommand_from agent-prompt" -l agent -r
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and __fish_seen_subcommand_from agent-prompt" -l generation -r
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and __fish_seen_subcommand_from agent-prompt" -l text -r
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and __fish_seen_subcommand_from agent-prompt" -l timeout-ms -d 'Maximum time to wait for a command response' -r
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and __fish_seen_subcommand_from agent-prompt" -l no-submit
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and __fish_seen_subcommand_from agent-prompt" -l pretty -d 'Pretty-print one-shot JSON responses. Streaming subscriptions stay JSON Lines'
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and __fish_seen_subcommand_from agent-prompt" -s h -l help -d 'Print help'
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and __fish_seen_subcommand_from agent-read" -l agent -r
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and __fish_seen_subcommand_from agent-read" -l generation -r
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and __fish_seen_subcommand_from agent-read" -l lines -r
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and __fish_seen_subcommand_from agent-read" -l timeout-ms -d 'Maximum time to wait for a command response' -r
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and __fish_seen_subcommand_from agent-read" -l pretty -d 'Pretty-print one-shot JSON responses. Streaming subscriptions stay JSON Lines'
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and __fish_seen_subcommand_from agent-read" -s h -l help -d 'Print help'
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and __fish_seen_subcommand_from agent-wait" -l agent -r
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and __fish_seen_subcommand_from agent-wait" -l generation -r
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and __fish_seen_subcommand_from agent-wait" -l state -r -f -a "idle\t''
running\t''
waiting-input\t''
attention\t''
finished\t''
failed\t''
settled\t'Any non-running terminal state'"
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and __fish_seen_subcommand_from agent-wait" -l after-seq -r
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and __fish_seen_subcommand_from agent-wait" -l timeout-ms -d 'Maximum time to wait for a command response' -r
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and __fish_seen_subcommand_from agent-wait" -l pretty -d 'Pretty-print one-shot JSON responses. Streaming subscriptions stay JSON Lines'
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and __fish_seen_subcommand_from agent-wait" -s h -l help -d 'Print help (see more with \'--help\')'
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and __fish_seen_subcommand_from subscribe" -l since -d 'Resume after this revision; the current snapshot is sent when newer' -r
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and __fish_seen_subcommand_from subscribe" -l timeout-ms -d 'Maximum time to wait for a command response' -r
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and __fish_seen_subcommand_from subscribe" -l pretty -d 'Pretty-print one-shot JSON responses. Streaming subscriptions stay JSON Lines'
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and __fish_seen_subcommand_from subscribe" -s h -l help -d 'Print help'
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and __fish_seen_subcommand_from new-window" -l timeout-ms -d 'Maximum time to wait for a command response' -r
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and __fish_seen_subcommand_from new-window" -l pretty -d 'Pretty-print one-shot JSON responses. Streaming subscriptions stay JSON Lines'
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and __fish_seen_subcommand_from new-window" -s h -l help -d 'Print help'
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and __fish_seen_subcommand_from focus" -l window -r
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and __fish_seen_subcommand_from focus" -l pane -r
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and __fish_seen_subcommand_from focus" -l timeout-ms -d 'Maximum time to wait for a command response' -r
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and __fish_seen_subcommand_from focus" -l pretty -d 'Pretty-print one-shot JSON responses. Streaming subscriptions stay JSON Lines'
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and __fish_seen_subcommand_from focus" -s h -l help -d 'Print help'
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and __fish_seen_subcommand_from new-tab" -l window -r
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and __fish_seen_subcommand_from new-tab" -l timeout-ms -d 'Maximum time to wait for a command response' -r
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and __fish_seen_subcommand_from new-tab" -l pretty -d 'Pretty-print one-shot JSON responses. Streaming subscriptions stay JSON Lines'
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and __fish_seen_subcommand_from new-tab" -s h -l help -d 'Print help'
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and __fish_seen_subcommand_from split" -l window -r
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and __fish_seen_subcommand_from split" -l pane -d 'Split this pane instead of the currently focused pane' -r
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and __fish_seen_subcommand_from split" -l direction -r -f -a "right\t'Create the new pane to the right of the focused pane'
down\t'Create the new pane below the focused pane'"
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and __fish_seen_subcommand_from split" -l timeout-ms -d 'Maximum time to wait for a command response' -r
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and __fish_seen_subcommand_from split" -l pretty -d 'Pretty-print one-shot JSON responses. Streaming subscriptions stay JSON Lines'
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and __fish_seen_subcommand_from split" -s h -l help -d 'Print help (see more with \'--help\')'
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and __fish_seen_subcommand_from prompt" -l window -r
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and __fish_seen_subcommand_from prompt" -l pane -r
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and __fish_seen_subcommand_from prompt" -l text -r
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and __fish_seen_subcommand_from prompt" -l wait -d 'After sending, wait until the pane reaches this task state' -r -f -a "idle\t''
running\t''
waiting-input\t''
attention\t''
finished\t''
failed\t''
settled\t'Any non-running terminal state'"
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and __fish_seen_subcommand_from prompt" -l timeout-ms -d 'Maximum time to wait for a command response' -r
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and __fish_seen_subcommand_from prompt" -l no-submit -d 'Write the text without appending Enter'
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and __fish_seen_subcommand_from prompt" -l pretty -d 'Pretty-print one-shot JSON responses. Streaming subscriptions stay JSON Lines'
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and __fish_seen_subcommand_from prompt" -s h -l help -d 'Print help (see more with \'--help\')'
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and __fish_seen_subcommand_from read" -l window -r
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and __fish_seen_subcommand_from read" -l pane -r
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and __fish_seen_subcommand_from read" -l lines -d 'Number of logical terminal rows to read from the buffer tail' -r
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and __fish_seen_subcommand_from read" -l timeout-ms -d 'Maximum time to wait for a command response' -r
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and __fish_seen_subcommand_from read" -l pretty -d 'Pretty-print one-shot JSON responses. Streaming subscriptions stay JSON Lines'
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and __fish_seen_subcommand_from read" -s h -l help -d 'Print help'
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and __fish_seen_subcommand_from procs" -l window -r
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and __fish_seen_subcommand_from procs" -l pane -r
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and __fish_seen_subcommand_from procs" -l timeout-ms -d 'Maximum time to wait for a command response' -r
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and __fish_seen_subcommand_from procs" -l pretty -d 'Pretty-print one-shot JSON responses. Streaming subscriptions stay JSON Lines'
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and __fish_seen_subcommand_from procs" -s h -l help -d 'Print help'
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and __fish_seen_subcommand_from send-key" -l window -r
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and __fish_seen_subcommand_from send-key" -l pane -r
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and __fish_seen_subcommand_from send-key" -l key -d 'Named key: escape, enter, arrows, navigation, f1-f12, or a-z with --control' -r
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and __fish_seen_subcommand_from send-key" -l repeat -r
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and __fish_seen_subcommand_from send-key" -l timeout-ms -d 'Maximum time to wait for a command response' -r
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and __fish_seen_subcommand_from send-key" -l shift
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and __fish_seen_subcommand_from send-key" -l alt
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and __fish_seen_subcommand_from send-key" -l control
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and __fish_seen_subcommand_from send-key" -l pretty -d 'Pretty-print one-shot JSON responses. Streaming subscriptions stay JSON Lines'
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and __fish_seen_subcommand_from send-key" -s h -l help -d 'Print help'
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and __fish_seen_subcommand_from run" -l window -r
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and __fish_seen_subcommand_from run" -l pane -r
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and __fish_seen_subcommand_from run" -l command -r
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and __fish_seen_subcommand_from run" -l timeout-ms -d 'Maximum time to wait for a command response' -r
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and __fish_seen_subcommand_from run" -l no-wait -d 'Submit the command and return its run id without waiting for completion'
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and __fish_seen_subcommand_from run" -l pretty -d 'Pretty-print one-shot JSON responses. Streaming subscriptions stay JSON Lines'
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and __fish_seen_subcommand_from run" -s h -l help -d 'Print help'
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and __fish_seen_subcommand_from wait" -l window -r
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and __fish_seen_subcommand_from wait" -l pane -r
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and __fish_seen_subcommand_from wait" -l state -r -f -a "idle\t''
running\t''
waiting-input\t''
attention\t''
finished\t''
failed\t''
settled\t'Any non-running terminal state'"
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and __fish_seen_subcommand_from wait" -l after-seq -d 'Require the pane\'s state_change_seq to advance past this value. Pass the value observed before sending work so an already-settled pane does not satisfy the wait immediately' -r
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and __fish_seen_subcommand_from wait" -l timeout-ms -d 'Maximum time to wait for a command response' -r
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and __fish_seen_subcommand_from wait" -l pretty -d 'Pretty-print one-shot JSON responses. Streaming subscriptions stay JSON Lines'
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and __fish_seen_subcommand_from wait" -s h -l help -d 'Print help (see more with \'--help\')'
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and __fish_seen_subcommand_from help" -f -a "describe" -d 'Describe the protocol version, runtime version, and available capabilities'
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and __fish_seen_subcommand_from help" -f -a "snapshot" -d 'Read the authoritative window, tab, pane, and task-state projection'
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and __fish_seen_subcommand_from help" -f -a "orchestrate" -d 'Execute one typed multi-step terminal workflow in a single Runtime request'
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and __fish_seen_subcommand_from help" -f -a "agents" -d 'List only panes Nebula recognizes as AI agents, with semantic state and session identity'
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and __fish_seen_subcommand_from help" -f -a "agent-start" -d 'Start a verified AI CLI in a new terminal tab and assign a stable name'
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and __fish_seen_subcommand_from help" -f -a "agent-fork" -d 'Create an isolated Git worktree, then start a named AI CLI in that checkout'
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and __fish_seen_subcommand_from help" -f -a "agent-get" -d 'Resolve one managed agent by stable id or active name'
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and __fish_seen_subcommand_from help" -f -a "agent-prompt" -d 'Send one plain-text prompt to a managed agent'
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and __fish_seen_subcommand_from help" -f -a "agent-read" -d 'Read the terminal-buffer tail owned by a managed agent'
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and __fish_seen_subcommand_from help" -f -a "agent-wait" -d 'Wait for the same managed-agent generation to reach a semantic state'
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and __fish_seen_subcommand_from help" -f -a "subscribe" -d 'Stream state snapshots whenever their semantic content changes'
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and __fish_seen_subcommand_from help" -f -a "new-window" -d 'Create and focus a new terminal window'
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and __fish_seen_subcommand_from help" -f -a "focus" -d 'Focus a window or one of its panes'
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and __fish_seen_subcommand_from help" -f -a "new-tab" -d 'Create a default-shell tab in the target window'
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and __fish_seen_subcommand_from help" -f -a "split" -d 'Split the focused pane in the target window'
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and __fish_seen_subcommand_from help" -f -a "prompt" -d 'Send one plain-text prompt to a pane, optionally submitting it with Enter'
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and __fish_seen_subcommand_from help" -f -a "read" -d 'Read the latest logical lines from a pane\'s real terminal buffer'
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and __fish_seen_subcommand_from help" -f -a "procs" -d 'List the real local process tree rooted at a pane\'s PTY shell'
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and __fish_seen_subcommand_from help" -f -a "send-key" -d 'Send a restricted named control key using the pane\'s active terminal mode'
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and __fish_seen_subcommand_from help" -f -a "run" -d 'Run one shell command and return its real OSC 133 exit status'
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and __fish_seen_subcommand_from help" -f -a "wait" -d 'Wait until a pane reaches a semantic task state'
complete -c nebula -n "__fish_nebula_using_subcommand ctl; and __fish_seen_subcommand_from help" -f -a "help" -d 'Print this message or the help of the given subcommand(s)'
complete -c nebula -n "__fish_nebula_using_subcommand migrate" -s c -l config-file -d 'Path to the configuration file' -r -F
complete -c nebula -n "__fish_nebula_using_subcommand migrate" -s d -l dry-run -d 'Only output TOML config to STDOUT'
complete -c nebula -n "__fish_nebula_using_subcommand migrate" -s i -l skip-imports -d 'Do not recurse over imports'
complete -c nebula -n "__fish_nebula_using_subcommand migrate" -l skip-renames -d 'Do not move renamed fields to their new location'
complete -c nebula -n "__fish_nebula_using_subcommand migrate" -s s -l silent -d 'Do not output to STDOUT'
complete -c nebula -n "__fish_nebula_using_subcommand migrate" -s h -l help -d 'Print help'
complete -c nebula -n "__fish_nebula_using_subcommand config; and not __fish_seen_subcommand_from check init help" -s h -l help -d 'Print help'
complete -c nebula -n "__fish_nebula_using_subcommand config; and not __fish_seen_subcommand_from check init help" -f -a "check" -d 'Validate a Lua, TOML, or YAML configuration without opening the GUI'
complete -c nebula -n "__fish_nebula_using_subcommand config; and not __fish_seen_subcommand_from check init help" -f -a "init" -d 'Create an annotated Lua configuration template'
complete -c nebula -n "__fish_nebula_using_subcommand config; and not __fish_seen_subcommand_from check init help" -f -a "help" -d 'Print this message or the help of the given subcommand(s)'
complete -c nebula -n "__fish_nebula_using_subcommand config; and __fish_seen_subcommand_from check" -l config-file -d 'Configuration file to validate; otherwise use normal discovery' -r -F
complete -c nebula -n "__fish_nebula_using_subcommand config; and __fish_seen_subcommand_from check" -s h -l help -d 'Print help'
complete -c nebula -n "__fish_nebula_using_subcommand config; and __fish_seen_subcommand_from init" -l config-file -d 'Lua configuration path; otherwise use the platform user config directory' -r -F
complete -c nebula -n "__fish_nebula_using_subcommand config; and __fish_seen_subcommand_from init" -l language -d 'Comment language for the generated template' -r -f -a "system\t''
zh-CN\t''
en-US\t''"
complete -c nebula -n "__fish_nebula_using_subcommand config; and __fish_seen_subcommand_from init" -l force -d 'Back up and replace an existing configuration'
complete -c nebula -n "__fish_nebula_using_subcommand config; and __fish_seen_subcommand_from init" -s h -l help -d 'Print help'
complete -c nebula -n "__fish_nebula_using_subcommand config; and __fish_seen_subcommand_from help" -f -a "check" -d 'Validate a Lua, TOML, or YAML configuration without opening the GUI'
complete -c nebula -n "__fish_nebula_using_subcommand config; and __fish_seen_subcommand_from help" -f -a "init" -d 'Create an annotated Lua configuration template'
complete -c nebula -n "__fish_nebula_using_subcommand config; and __fish_seen_subcommand_from help" -f -a "help" -d 'Print this message or the help of the given subcommand(s)'
complete -c nebula -n "__fish_nebula_using_subcommand notify-test" -s h -l help -d 'Print help'
complete -c nebula -n "__fish_nebula_using_subcommand setup-ai" -l remove -d 'Remove Nebula\'s hooks from claude\'s settings.json instead of installing them'
complete -c nebula -n "__fish_nebula_using_subcommand setup-ai" -s h -l help -d 'Print help'
complete -c nebula -n "__fish_nebula_using_subcommand ssh" -s h -l help -d 'Print help'
complete -c nebula -n "__fish_nebula_using_subcommand help; and not __fish_seen_subcommand_from ctl migrate config notify-test setup-ai ssh help" -f -a "ctl" -d 'Agent-oriented terminal control: split panes, run commands, start Codex/Claude, send prompts, wait for state changes, and read verified terminal output'
complete -c nebula -n "__fish_nebula_using_subcommand help; and not __fish_seen_subcommand_from ctl migrate config notify-test setup-ai ssh help" -f -a "migrate" -d 'Migrate the configuration file'
complete -c nebula -n "__fish_nebula_using_subcommand help; and not __fish_seen_subcommand_from ctl migrate config notify-test setup-ai ssh help" -f -a "config" -d 'Validate or create the Nebula configuration'
complete -c nebula -n "__fish_nebula_using_subcommand help; and not __fish_seen_subcommand_from ctl migrate config notify-test setup-ai ssh help" -f -a "notify-test" -d 'Test system notification (toast) delivery'
complete -c nebula -n "__fish_nebula_using_subcommand help; and not __fish_seen_subcommand_from ctl migrate config notify-test setup-ai ssh help" -f -a "setup-ai" -d 'Install (or --remove) AI hooks plus the Nebula Runtime Skill for Codex and Claude Code'
complete -c nebula -n "__fish_nebula_using_subcommand help; and not __fish_seen_subcommand_from ctl migrate config notify-test setup-ai ssh help" -f -a "ssh" -d 'SSH with Nebula shell integration bootstrapped on the remote host, so tab icons / spinner / cwd track the program running over the connection (claude, vim, cargo…). All arguments are forwarded to the system `ssh`'
complete -c nebula -n "__fish_nebula_using_subcommand help; and not __fish_seen_subcommand_from ctl migrate config notify-test setup-ai ssh help" -f -a "help" -d 'Print this message or the help of the given subcommand(s)'
complete -c nebula -n "__fish_nebula_using_subcommand help; and __fish_seen_subcommand_from ctl" -f -a "describe" -d 'Describe the protocol version, runtime version, and available capabilities'
complete -c nebula -n "__fish_nebula_using_subcommand help; and __fish_seen_subcommand_from ctl" -f -a "snapshot" -d 'Read the authoritative window, tab, pane, and task-state projection'
complete -c nebula -n "__fish_nebula_using_subcommand help; and __fish_seen_subcommand_from ctl" -f -a "orchestrate" -d 'Execute one typed multi-step terminal workflow in a single Runtime request'
complete -c nebula -n "__fish_nebula_using_subcommand help; and __fish_seen_subcommand_from ctl" -f -a "agents" -d 'List only panes Nebula recognizes as AI agents, with semantic state and session identity'
complete -c nebula -n "__fish_nebula_using_subcommand help; and __fish_seen_subcommand_from ctl" -f -a "agent-start" -d 'Start a verified AI CLI in a new terminal tab and assign a stable name'
complete -c nebula -n "__fish_nebula_using_subcommand help; and __fish_seen_subcommand_from ctl" -f -a "agent-fork" -d 'Create an isolated Git worktree, then start a named AI CLI in that checkout'
complete -c nebula -n "__fish_nebula_using_subcommand help; and __fish_seen_subcommand_from ctl" -f -a "agent-get" -d 'Resolve one managed agent by stable id or active name'
complete -c nebula -n "__fish_nebula_using_subcommand help; and __fish_seen_subcommand_from ctl" -f -a "agent-prompt" -d 'Send one plain-text prompt to a managed agent'
complete -c nebula -n "__fish_nebula_using_subcommand help; and __fish_seen_subcommand_from ctl" -f -a "agent-read" -d 'Read the terminal-buffer tail owned by a managed agent'
complete -c nebula -n "__fish_nebula_using_subcommand help; and __fish_seen_subcommand_from ctl" -f -a "agent-wait" -d 'Wait for the same managed-agent generation to reach a semantic state'
complete -c nebula -n "__fish_nebula_using_subcommand help; and __fish_seen_subcommand_from ctl" -f -a "subscribe" -d 'Stream state snapshots whenever their semantic content changes'
complete -c nebula -n "__fish_nebula_using_subcommand help; and __fish_seen_subcommand_from ctl" -f -a "new-window" -d 'Create and focus a new terminal window'
complete -c nebula -n "__fish_nebula_using_subcommand help; and __fish_seen_subcommand_from ctl" -f -a "focus" -d 'Focus a window or one of its panes'
complete -c nebula -n "__fish_nebula_using_subcommand help; and __fish_seen_subcommand_from ctl" -f -a "new-tab" -d 'Create a default-shell tab in the target window'
complete -c nebula -n "__fish_nebula_using_subcommand help; and __fish_seen_subcommand_from ctl" -f -a "split" -d 'Split the focused pane in the target window'
complete -c nebula -n "__fish_nebula_using_subcommand help; and __fish_seen_subcommand_from ctl" -f -a "prompt" -d 'Send one plain-text prompt to a pane, optionally submitting it with Enter'
complete -c nebula -n "__fish_nebula_using_subcommand help; and __fish_seen_subcommand_from ctl" -f -a "read" -d 'Read the latest logical lines from a pane\'s real terminal buffer'
complete -c nebula -n "__fish_nebula_using_subcommand help; and __fish_seen_subcommand_from ctl" -f -a "procs" -d 'List the real local process tree rooted at a pane\'s PTY shell'
complete -c nebula -n "__fish_nebula_using_subcommand help; and __fish_seen_subcommand_from ctl" -f -a "send-key" -d 'Send a restricted named control key using the pane\'s active terminal mode'
complete -c nebula -n "__fish_nebula_using_subcommand help; and __fish_seen_subcommand_from ctl" -f -a "run" -d 'Run one shell command and return its real OSC 133 exit status'
complete -c nebula -n "__fish_nebula_using_subcommand help; and __fish_seen_subcommand_from ctl" -f -a "wait" -d 'Wait until a pane reaches a semantic task state'
complete -c nebula -n "__fish_nebula_using_subcommand help; and __fish_seen_subcommand_from config" -f -a "check" -d 'Validate a Lua, TOML, or YAML configuration without opening the GUI'
complete -c nebula -n "__fish_nebula_using_subcommand help; and __fish_seen_subcommand_from config" -f -a "init" -d 'Create an annotated Lua configuration template'
