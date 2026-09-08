# Connections: Termius reference

Inspected on 2026-09-05. This records the reference for the Jarvis Machines/VM redesign, not a claim that Jarvis implements Termius features.

## Official sources and observed anatomy

- [Termius desktop product demonstration](https://termius.com/index.html): inspected the live macOS demonstration in the browser, including the Hosts inventory and the Host Details panel. Hosts appear as compact tiles with a colored icon, readable name, and one secondary line. Groups sit above hosts. The inventory toolbar offers a New Host action. A narrow inspector on the right keeps the selected host's address, name/tags, SSH port, and credentials together, followed by one prominent Connect action. The inventory remains visible beside the inspector. The product demonstration is illustrative; no account or real remote connection was opened.
- [Termius desktop navigation redesign](https://termius.com/blog/termius-x), published 2024-06-27: Termius explicitly separates saved infrastructure from active terminals, and describes search-driven keyboard navigation for opening or switching connections. This supports maintaining a clear distinction between managing a machine and entering its active terminal. It does not justify replacing Jarvis's existing launcher navigation with Termius tabs.
- [Termius connection setup guidance](https://termius.com/blog/prepare-to-work-from-home), published 2020-03-27: the host is the saved connection object; advanced access configuration belongs in its editor. This older source supports the object model only, not current visual details.

## Five decisions for Jarvis

These are implementation recommendations inferred from the reference and the user's screenshot.

1. **Make the overview an inventory.** Put the title, search, and one visible “Добавить машину” button at the top. Represent each existing machine with a compact selectable card: icon, friendly name, transport, and truthful status. Keep the existing local VM distinction as a small separate collection. A lone saved machine must still look intentional rather than spread over a page.
2. **Show one machine's details at a time.** Selecting a card opens an inspector beside the inventory. Use the friendly host name as its heading and show the full target address in a compact copyable row. Put agent profiles and maintenance inside this inspector. Remove nested disclosures from the overview so details do not push unrelated machines down the page.
3. **Use the inspector for focused creation and editing.** Start with connection type, name, and address. Show SSH credentials only for SSH, and Teleport cluster/host choices only for Teleport. Give every field a persistent label; align fields on one grid. Keep optional port, jump host, paths, and diagnostics behind a single advanced section. Preserve the existing preflight/setup sequence and authentication behavior.
4. **Make the next action obvious.** Give the inspector one primary action based on actual state: connect/setup/reconnect. Keep checking, editing, and maintenance secondary. Group status text beside its icon or badge; do not strand a status label at the far edge of a row. Surface setup progress and actionable errors in the inspector near the action that caused them.
5. **Use restrained visual contrast and spatial continuity.** Keep the page on graphite, lift machine cards slightly, and give the inspector its own darker or lighter surface. Use color for transport icons, selected state, connection health, and the primary action. Rounded fields and subtle borders support grouping. Animate inspector entry and state changes briefly; respect reduced motion. On narrow windows, the inspector becomes the current view with Back/Escape returning to the inventory and restoring selection.

## Review criteria

- At a glance, the page answers which machines exist, which is selected, whether it is reachable, and what can be done next.
- No raw UUID or long address dominates the inventory; the full target remains accessible in details.
- Creating a connection does not expand a long form between existing machine rows.
- Advanced settings are available without competing with the normal connection path.
- Selection, keyboard focus, loading, empty, failure, and connected states remain distinct in both themes.
- This is a presentation change: no new vault, credential storage, grouping, terminal transport, or synchronization feature is implied.
