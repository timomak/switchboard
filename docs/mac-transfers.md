# Transfer conversations between Macs

Export a selected conversation or a project's selected chats, move the package to
another Mac, then import it into Switchboard there. Choose the receiving Mac's
existing project folder and destination app. Creation uses the same native
adapters and read-back checks as [Continue in another app](continue-in-another-app.md).
The source remains unchanged. Later edits on the two Macs are independent.

An explicitly saved package can travel through iCloud Drive, AirDrop or another
private file transfer. Switchboard does not watch it, merge later edits, discover
other computers or replicate databases in the background. No Tailscale connection
is required by the package format.

## Use a transfer

1. On the source Mac, open **Continue in another app…**, select a chat and choose
   **Export for another Mac…**. For several project conversations, open
   **Clone project…**, select the project/chats and choose **Export selected chats…**.
2. Save the `.switchboard-transfer` file to a private location and transfer it to
   the other Mac. Review its text before sharing.
3. In the matching chat or project flow on the receiving Mac, choose
   **Import transfer…**. Review the conversations and omissions, choose the
   destination app and an existing local project folder, then create the copy.
4. Use the result's Open action when ready. Importing a file alone does not create
   or open native chats, submit a prompt, or run a model request.

The versioned format accepts bounded transcript fields only. Unknown fields,
unsupported versions, invalid roles and selections above the limits are rejected.
Limits are 200 chats, 40 MB aggregate text, 10,000 messages and 5 MB text per chat,
and 100 MB serialized input. Duplicate transcripts in a project selection are
rejected with a request to export one copy of each. There is no silent truncation.
Native creation saves local receipts for retry; importing again as a new request deliberately creates
another independent copy.

## Chat and project scope

A chat package contains the selected historical message roles, text, timestamps, a
title and omission notes. An explicitly supplied summary or next step is included
as a supplemental user message. A project package groups selected conversations
under a name.
It does not recreate the destination app's project registry or sidebar grouping.
Every accepted conversation gets a fresh native session on the receiving Mac.

Supported native destinations are Codex Desktop, Codex CLI, Claude Code and Claude
Desktop Code. The adapters have the same version and sign-in requirements as local
cloning. Cowork and ordinary Claude Desktop Chat do not gain native import support.

Set up the project folder or Git checkout on the receiving Mac first. Packages do
not contain working files, Git state, dependencies, tools, project instructions,
configuration, native attachments or active jobs. The receiving folder becomes the
new session's working directory. Literal paths inside old message text are kept
as recorded; Switchboard does not rewrite historical commands or assert that the
two project folders contain the same code.

Account stores, cookies, tokens and Keychain entries are not exported. Conversation
text can itself contain private information or pasted secrets; review what you
export and use a private destination. A package is not encrypted by Switchboard.

## Authentication

The receiving app uses its own existing login. Chat and project packages cannot
sign it in. Switchboard does not synchronize credentials between Macs.

Official remote authorization can reduce repeat login work: Codex CLI has device
login approved in a browser, and Claude Code can show a login URL and accept the
resulting browser code when used over SSH. These create a destination login and
can still require consent or MFA. They are separate from Desktop app login and
are not a new Switchboard remote-login feature in this release.

See [Codex authentication](https://learn.chatgpt.com/docs/auth#login-on-headless-devices),
[Claude Code authentication](https://code.claude.com/docs/en/authentication) and
[Claude account login](https://support.claude.com/en/articles/13189465-log-in-to-your-claude-account).
