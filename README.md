# PlaneAI Linear plugin

Mirror the Linear issues you work on as PlaneAI tasks, assign them to projects from the sidebar, and move them in Linear as the work progresses.
It is a local PlaneAI plugin: a Rust sidecar plus browser UI bundles, with the same features as PlaneAI's builtin Jira plugin.

## Requirements

- PlaneAI with plugin host API `planeai.plugin-host.v7`.
  The plugin relies on the `tasks.sync` capability (tasks it creates are owned by it) and on the v7 frame bridge (`interaction.showToast`, `interaction.openProjectForm`, `data.onTaskDataChanged`).
  Older PlaneAI releases refuse to install the package.
- A Linear workspace, reached with a personal API key or with OAuth.

## Install

1. Download the archive for your platform from the latest release, or build it yourself with `make package`.
2. In PlaneAI, open **Preferences → Plugins**, choose **Install local package**, and select the `planeai-plugin-linear` directory.
3. Enable the plugin, then open **Preferences → Linear** to connect.

Plugins are trusted native code that runs with your user's permissions, so install packages only from a source you trust.

## Connect

The preferences page offers two ways to connect, and both store the result in the plugin's secrets directory.

**Personal API key.**
Create a key in Linear under Settings, Security & access, Personal API keys, paste it, and choose **Connect**.
The plugin checks the key by asking Linear for the key's workspace (`viewer { organization { id name urlKey } }`) and saves it only when Linear accepts it.
This needs no app registration.

**OAuth.**
Builds compiled with `LINEAR_CLIENT_ID` also show **Connect with Linear**.
It runs the authorization code flow with PKCE (`S256`) against `https://linear.app/oauth/authorize` and `https://api.linear.app/oauth/token`, with scopes `read,write` and `actor=user`, and no client secret.
Linear redirects to a loopback listener on `http://localhost:19297/callback`, so register that redirect URI in your Linear OAuth app.
Linear access tokens last 24 hours: the plugin renews one with its refresh token five minutes before it expires, or once after Linear answers 401, and saves the rotated refresh token.
When Linear rejects the refresh token (`invalid_grant`), the plugin deletes the credentials and asks you to reconnect.
A long-lived token without a refresh token is used as is.
Builds without a client id hide the OAuth button and offer only the API key.

Disconnect deletes the credentials.
Connecting another workspace wipes everything the plugin cached for the previous one at the next sync.

## Sources

A source is a named Linear query whose issues become PlaneAI tasks.
It is either a custom view or a raw `IssueFilter`, Linear's analog of a Jira JQL query.

- **Custom view.**
  Paste the view's URL, such as `https://linear.app/acme/view/my-issues-0a1b2c3d4e5f`, its slug id `0a1b2c3d4e5f`, or its uuid.
  The plugin reads `customView(id:).issues`.
- **Issue filter.**
  Write an `IssueFilter` JSON object, which the plugin passes to `issues(filter:)`.
  A new source starts with the open issues assigned to you or to no one: `{"or":[{"assignee":{"isMe":{"eq":true}}},{"assignee":{"null":true}}],"state":{"type":{"nin":["completed","canceled"]}}}`.

The settings document PlaneAI stores for the plugin looks like this:

```json
{
  "sync_interval_ms": 60000,
  "sources": {
    "mine": {
      "query": { "kind": "view", "view": "https://linear.app/acme/view/my-issues-0a1b2c3d4e5f" },
      "status_map": { "In QA": "in_review" },
      "writeback": { "on_start": "In Progress", "on_complete": "Done", "comment": true }
    },
    "platform": {
      "query": { "kind": "filter", "filter": { "team": { "key": { "eq": "PLT" } } } },
      "status_map": {},
      "writeback": null
    }
  },
  "key_prefixes": { "TRA": "LTRA" }
}
```

The sidecar validates every update and rejects malformed settings instead of guessing: an unknown field, a blank or malformed view, a filter that is not a non-empty object, a status that is not `todo`, `in_progress`, `in_review` or `done`, a blank writeback state, an interval under 10 seconds, or a key prefix that is not 1 to 10 uppercase letters and digits starting with a letter, or that two teams would share.
Renaming a source keeps its issues; removing one lets its issues go quietly, without asking you about them.

## Status and priority mapping

A task's status comes from the issue's workflow state.
An entry in the source's `status_map` for the state's name wins.
Otherwise the state's type decides: `triage`, `backlog` and `unstarted` map to `todo`, `started` to `in_progress`, and `completed`, `canceled` and `duplicate` to `done`.

PlaneAI dispatches larger priorities first, so Linear's priorities are reversed: urgent (1) becomes 4, high (2) becomes 3, medium (3) becomes 2, low (4) becomes 1, and no priority (0) stays 0.
Labels become the task's tags.

## Sync and departures

PlaneAI calls `linear.syncNow` every `sync_interval_ms` (60 seconds by default), and **Sync Now** runs it on demand.
For each source, in name order, the plugin fetches every issue in pages of 50 and stops with an error past 5,000.
It creates a task keyed by the issue's identifier (`ENG-123`) when none exists, and updates a linked task only when its title, description, status, priority or tags changed.
Descriptions longer than 24 KiB are truncated with a note pointing to Linear.
The task keeps the key it was created with when the issue moves to another team and changes identifier, so its children stay attached.
A local task that already uses the key and was not created by the plugin is never overwritten.
The sync skips that issue, keeps syncing the rest of its source, and lists the skipped keys under Sync Now.
This happens when a Linear team key matches a PlaneAI project prefix, for an issue whose number the project already used.
Sync Now then offers a prefix for that team, such as `LTRA` for `TRA`, saves it in `key_prefixes`, and syncs again.
The team's issues then sync as tasks such as `LTRA-13`, and the Linear sidebar still shows them as `TRA-13`.
Tasks already synced keep their keys when a prefix is added or removed, and Preferences lists each prefix with a way to remove it.
An issue whose mapped key another synced issue already holds, such as `OPS-1` mapped to `ENG-1` while team `ENG` syncs, is skipped the same way instead of sharing that task.
One failing source never stops the others; the totals list each error.

An issue that leaves every source's results is a departure.
After a sync in which every source succeeded, the plugin queues the departed issues whose task is not done yet, and PlaneAI shows them one at a time as a toast, "`ENG-123` left your Linear view".
**Done** (or Enter) marks the task done, and the plugin dequeues the issue only after PlaneAI saved that.
Dismissing the toast dequeues the issue and leaves the task alone.
A failed sync never rebuilds the queue, so an issue moving between two sources never prompts.

## Sidebar and assignment

The Linear section of PlaneAI's sidebar lists the synced issues with their identifier, title and status, with a count in its header.
It collapses, follows PlaneAI's sidebar keyboard navigation, and refreshes when the plugin's data or PlaneAI's tasks change.

Selecting an issue opens the **Assign Linear issue** dialog.
It previews the issue, lets you pick a PlaneAI project (P focuses the picker) or create one (N opens PlaneAI's project form), and Mod+Enter creates a child task of the issue's task in that project.
Escape returns from a field to the dialog, then closes it.

## Writeback

Writeback is per source, set in the source's **Write back to Linear** fields.

- When the first child task is assigned to an issue's task, the plugin moves the issue to `on_start`.
- When the task moves to done from any other status, including when PlaneAI completes a parent automatically, it moves the issue to `on_complete`.
- With `comment` on, it also comments `planeai: Task moved to Start|Complete at <UTC time>`.

The state is the issue's team workflow state whose name matches, ignoring case.
An issue in several sources uses the writeback of the first one by name that still returns it.
The move and the comment fail independently, and the dashboard shows the last outcome.

## Durable state

- PlaneAI's settings document holds the sources and interval; the sidecar validates every write and restores the previous document when it cannot apply one.
- `PLANEAI_PLUGIN_SECRETS_DIR/credentials.json` holds the API key or OAuth tokens and the workspace, written atomically with mode 0600.
- `PLANEAI_PLUGIN_DATA_DIR/linear.sqlite` holds the cache, with versioned migrations: issues by uuid, source memberships, issue-to-task links, the departure queue, the cached workspace id, and a snapshot of the last applied settings, which lets the plugin catch up on renames and removals made while it was not running.

PlaneAI deletes all of it when you remove the plugin.

## Development

You need Rust (stable), Node 22 or later, and pnpm 10.28.2.

```bash
pnpm install --frozen-lockfile
make test            # rustfmt, clippy, oxfmt, oxlint, Rust tests, UI tests in jsdom, manifest and release contract tests
make fmt             # format Rust and TypeScript
make package         # stage dist/planeai-plugin-linear for this machine
make verify-package  # stage, then check the binary's handshake against the manifest
```

Set `LINEAR_CLIENT_ID` when building to enable OAuth.
The sidecar tests talk to a mocked Linear (wiremock) and to a fake PlaneAI host over the real JSON-RPC pipe.
With a `planeai-cli` built from a v7 PlaneAI, run the host's contract test with `planeai-cli plugin test --package dist/planeai-plugin-linear`.

Do not start a second PlaneAI app to try the plugin: it would share the running app's session server and close its sessions.

## Release

Pushes to `main` run the release workflow.
`auto` derives the version bump from Conventional Commit messages, tags the release, builds and tests on macOS (arm64 and x64), Linux x64 and Windows x64, verifies each staged handshake, and uploads one archive per platform.
The release build reads the OAuth client id from the repository variable `LINEAR_CLIENT_ID`.
