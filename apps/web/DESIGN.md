# Pumpkin Manager — Admin UI Design Plan (Phase 7, Task 3)

## Subject / Audience / Job

- **Subject:** home Minecraft server ops run by non-experts. The operator is
  often a parent, friend, or hobbyist — not a sysadmin. They know "the server
  needs the new Pumpkin version" but not what an image digest is.
- **Audience:** two tiers. Owners/admins see Users, Audit, Settings, and the
  per-server Update card. Viewers keep Overview/Servers/Monitoring only and get
  an explicit denied state (never a silent 404) if they reach for an admin URL.
- **Job:** let an owner (1) change who can touch the panel, (2) see who did
  what, (3) back up / stage-restore the manager database, and (4) move a server
  to a new image and back — without ever bricking the last owner account,
  losing data silently, or stranding a viewer on a blank page.

## Tokens (extend `index.css`, don't reinvent it)

The house language is dark minimalist + restrained green:
`#0c1110` page, `#e6efe9` text, `#141c19` card, `#22332b` hairline,
`#93a399` muted, `#7dd8a8` green, `#2c6b4a` ok-border, `#e8c87a` warn,
`#f0a3a3` err text, `#8a2f2f` err-border, `#0a0f0d` log well.
Admin work adds exactly one new wash and names the rest so future pages reuse
them instead of inventing hex:

| Token | Hex | Role |
|---|---|---|
| `--pm-ink` | `#e6efe9` | primary text (existing body color, named) |
| `--pm-fog` | `#93a399` | secondary/muted text (existing `.muted`, named) |
| `--pm-pine` | `#7dd8a8` | action + ok accent (existing nav/badge green, named) |
| `--pm-amber` | `#e8c87a` | caution text — preview warnings, pre-restore notes (existing `.warn`, named) |
| `--pm-ember` | `#f0a3a3` | destructive text + errors (existing `.err`, named) |
| `--pm-ember-wash` | `#1c1214` | danger-zone card wash — tinted backdrop behind destructive actions so the danger zone reads as one bounded region, distinct from normal `#141c19` cards |

No new font families except the Data role below (justified there). No shadows,
no gradients, no accent hues beyond the wash: destructive meaning travels
through `--pm-ember` text + border, never color alone.

## Type roles (existing system stack)

`font-family: system-ui, sans-serif` stays the single UI stack. Four roles:

- **Title** — page `h2`, semibold, default size. One per page ("Users",
  "Audit log", "Settings", "Update image"). Says where you are.
- **Section** — `h3`/`h4`. Groups cards ("New user", "Preview", "Manager
  database"). Scannable with a keyboard / screen reader heading jump.
- **Body** — default text, `.muted` for explanations. Every destructive or
  irreversible action gets one plain Body sentence saying what happens
  ("Restoring replaces current server data.").
- **Data** — `ui-monospace, SFMono-Regular, Menlo, Consolas, monospace` inside
  `<code>` for image references, digests, task ids, user ids, timestamps.
  Justification for the one exception: digests and ids are
  character-compared by humans (`sha256:9f2…` vs `sha256:9f3…`); proportional
  type makes that error-prone. Everything else stays system-ui.

Statuses are always **text + color** (`succeeded` in a badge AND the word
itself), never a bare dot — colorblind-safe and greppable.

## Layout concept + ASCII wireframes

Shared skeleton for all three pages: Title → error/loading state → working
content → danger at the bottom. New pages reuse `.shell`, `.row`, `.table`,
`.card`, `.badge`, `.log`, `.muted/.err/.warn` — zero new layout primitives.

### Users table

```
Users
Manage who can sign in. Deactivating revokes sessions immediately. [you are "sam (owner)"]

LAST_OWNER banner (only after a refused demote/delete):
[!] Cannot demote or deactivate the last active owner.

username      role      active   created      last login   actions
sam (you)     owner     yes      2026-09-01   2h ago       [Deactivate(disabled: last owner)]
ana           admin     yes      2026-10-02   yesterday    [Demote▾] [Deactivate] [Delete]
bob           viewer    no       2026-10-10   —            [Activate] [Delete]

New user                                    Danger zone (bottom, ember-wash card)
[username____] [password____] [role: viewer▾]  Deleting a user cannot be undone.
[Create user]                                   Deleting yourself signs you out immediately.
weak-password / USER_EXISTS errors appear here as text, not toasts.
```

### Audit timeline

```
Audit log
Newest first. Metadata is redacted server-side; secrets never appear here.

[action filter: _________ ] [actor search: ___ ] (prev/next page buttons)

time                actor      action          target            outcome
2026-10-16 14:02    sam        users.update    user:ana          ok
2026-10-16 13:58    sam        server.update   server:lobby      ok  [details: {"image_reference":"…"}]
2026-10-16 13:40    —          auth.login      user:bob          denied

[< Prev] [Next >]  (page state in words: "showing 50–100", never just arrows)
```

Rows are a newest-first ledger, not a marketing "activity feed": monospace ids,
plain-English outcome words (`ok` / `denied` / `failed`), metadata shown as
redacted `<code>` text.

### Update card (inside ServerDetail, below Backups)

```
Update image
Current:  pumpkin/mc:1.21 <code>sha256:9f2c…</code>   Previous: pumpkin/mc:1.20 (rollback target)
[target image: pumpkin/mc:1.22______] [digest (optional): sha256:___] [Preview]

Preview warnings (text list, always words first):
[!] No digest pinned — tag may drift between pulls.
[!] No verified backup found — the update is refused until you create one.
[Update to this image]   (409 → inline: "No verified backup. [Create a backup first → #backups]")

Update task abc123: [succeeded] 42% — pulling image…
[!] Rollback is one-way for data: world files written by the new version may not
    load on the old image. Always keep a backup.
[Roll back to pumpkin/mc:1.20]   (disabled + reason when no previous image)
```

### Settings cards

```
Settings
Manager database — the panel's own data (users, servers, schedules), not worlds.

[Manager database card]
  Download a consistent snapshot (.db).     [Download backup]
  Procedure: download → stage a restore file → restart applies it.
  A pre-restore copy (.pre-restore-<ts>) is kept automatically. See docs/backup-restore.md.

[Staged restore card]
  [choose .db file] [Stage restore]
  [i] Staged — restart the manager to apply. Live data is untouched until restart.

[Danger zone card — bottom, ember-wash]
  Restoring replaces the live manager database on next restart.
  Keep a downloaded backup before staging anything.
```

## Principles (what makes THIS distinct from generic SaaS)

1. **Server-console vernacular, not SaaS euphemism.** Buttons say "Run full
   backup", "Stage restore", "Roll back to …" — the words a Minecraft host
   uses. No "Sync", "Deploy", "Provision".
2. **Danger lives at the bottom, in one bounded wash.** Delete-user, staged
   restore consequences, and rollback all sit last on their page/section inside
   the ember wash, each with one plain sentence of consequence. Destructive
   actions never headline.
3. **Text first, color second.** Every status is a word (`ok`, `denied`,
   `staged`, `restart required`) plus the house badge; color never carries
   meaning alone.
4. **Keyboard-first, no pointer-only flows.** Native buttons/inputs/selects,
   visible `:focus-visible` (existing green ring), double-confirm restores via
   real dialogs — the Backups-section arm/confirm pattern, not hover menus.
5. **Defensive against a backend landing in parallel.** Every contract read
   tolerates missing keys (`previous_image_reference` may be absent until the
   controller wires it → rollback button shows "no previous image" instead of
   crashing). Loading and error states are designed, not `?.`-shaped holes.
6. **Reduced motion respected.** No animation, no auto-advancing carousels;
   polling is text updates ("42% — pulling image…") that screen readers and
   `prefers-reduced-motion` users perceive identically.

## Review

Self-review against the brief; each axis below first read as the generic
default and was revised with reason:

1. **Tokens read as "new palette" → revised to name-then-add.** First draft
   introduced fresh danger reds and a new surface scale. Revised: five of six
   tokens are existing `index.css` hex given names; exactly one new hex
   (`--pm-ember-wash`) exists, because the danger zone needs a bounded region
   and reusing `#141c19` would make it visually identical to safe cards —
   dangerous actions must not look like safe ones.
2. **Type read as "import Inter + JetBrains Mono" → revised to roles on the
   stack.** Generic SaaS would pull webfonts. Revised: system-ui only, mono
   via platform stacks and only for the Data role; a home-server panel must
   work offline on a LAN with zero font CDN dependency.
3. **Audit read as "activity feed with avatars" → revised to ledger
   wireframe.** Avatars/initials/relative-only timestamps are the SaaS
   default. Revised: monospace ids, absolute UTC-ish timestamps, outcome words
   — this page exists for incident forensics ("who demoted whom before the
   outage"), not engagement.
4. **Users read as "role badges + inline magic" → revised to explicit guard
   banner.** Generic admin would silently disable buttons. Revised: the
   LAST_OWNER refusal surfaces as a persistent banner explaining *why*, and
   self-deletion states its consequence ("signs you out immediately") —
   non-expert operators must never guess why a control refused them.
5. **Update read as "one-click Deploy button" → revised to preview-first
   card.** SaaS default hides consequences behind a single CTA. Revised:
   Preview → warnings-as-sentences → Update → task poll → rollback with a
   permanent one-way-data warning; the 409 path links to backup creation
   instead of dead-ending, because the safe path must be one click away at the
   moment of refusal.
6. **Settings read as "danger-zone theater (big red page)" → revised to
   procedure-first cards.** Generic SaaS makes Settings a wall of red
   buttons. Revised: download/procedure/stage flow comes first in calm cards;
   only consequences sit in the wash at the bottom — irreversibility is
   explained, not shouted.
