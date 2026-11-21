# Page Contract: Krabbit Web UI

## URL Structure

| URL | Page | Data |
| --- | ---- | ---- |
| `/` | Homepage | Holes list (filtered, paginated) |
| `/holes/{hole_id}` | Hole detail | Hole + Shells |
| `/shells/{shell_id}` | Shell detail | Shell + Digs + Buries |
| `/krabbers` | Leaderboard | Top 50 contributors |
| `/krabbers/{user_id}` | Contributor profile | User's contributions |

## Query Parameters

### Homepage (`/`)

```
?status=contested         Filter by Hole status
?domain=inference         Filter by domain tag
?cursor=<opaque>          Pagination cursor
```

## Page Content

### Homepage

- **Header**: Krabbit logo, nav (Home, Leaderboard)
- **Filters**: Status dropdown, domain tag chips
- **Hole cards**: Each shows:
  - Title (linked to detail)
  - Status badge (colored: open=gray, contested=amber,
    holding=green, settled=blue)
  - Domain tags as chips
  - Shell count, Dig total, Bury total
  - Last activity relative time ("2 hours ago")
- **Pagination**: "Load more" or next/prev cursor links
- **Empty state**: "No Holes yet" + explanation of Krabbit

### Hole Detail (`/holes/{id}`)

- **Hole header**: Title, status badge, description, domain tags,
  author entity, creation date
- **Stats bar**: Shell count, total Digs, total Buries
- **Shells list**: Each Shell card shows:
  - Claim text (truncated with expand)
  - Artifact hash (monospace, truncated)
  - Dig count / Bury count badges
  - Author entity
  - Link to Shell detail
- **Sub-Holes**: Listed if any exist

### Shell Detail (`/shells/{id}`)

- **Shell header**: Full claim, artifact hash, artifact size,
  author entity, creation date
- **Lineage**: Link to predecessor if exists
- **Digs section**: List of individual Digs:
  - Environment (monospace block)
  - Output hash
  - Duration
  - Author entity
  - "Author Dig" badge if is_author_dig
  - Timestamp
- **Buries section**: List of individual Buries:
  - Failure reason
  - Conditions
  - Reproduction steps (preformatted)
  - Author entity
  - Timestamp

### Leaderboard (`/krabbers`)

- **Table/cards**: Ranked list showing:
  - Rank number
  - Display name (linked to profile)
  - Entity (org/domain)
  - Total Digs
  - Total Shells
  - Total Buries
- **Limit**: Top 50

### Contributor Profile (`/krabbers/{user_id}`)

- **Header**: Display name, entity, join date
- **Stats**: Total Digs, Shells, Buries
- **Recent activity**: List of recent Holes created, Shells
  dropped, Digs and Buries submitted (mixed timeline, most
  recent first)

## Visual Design

- **Theme**: Dark background (#0d1117), light text (#e6edf3),
  accent crab-orange (#f97316)
- **Status colors**: Open=#6b7280, Contested=#f59e0b,
  Holding=#22c55e, Settled=#3b82f6
- **Font**: System font stack (no external font loading)
- **Responsive**: Flexbox/grid, mobile-first, breakpoints at
  480px, 768px, 1024px
