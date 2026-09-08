# Notification motion regression — 2026-09-05

The reported rectangular halo had a concrete CSS cause: `.card:hover` / `.card.hot` restored the main panel's `0 20px 48px` shadow. This selector outranked the compact notification shadow, whose transparent canvas previously had only 12px side / 10px bottom gutters. The large shadow was clipped to the rectangular WebView. Notification surfaces now use one compact shadow in both idle and hovered states, inside 24px top/side and 28px bottom transparent gutters. There is no full-window backdrop filter.

Recording, processing and result now share a persistent card shell. The production `analyzing` phase keeps the compact waveform layout. New phases animate the actual width, height and corner radius for 340ms while old and new content crossfade. Entry waits for native resize acknowledgement; exit finishes before native hide. Elapsed-time updates preserve the same waveform node and animation clock. A new recording during dismissal cancels the old dismissal without duplicating the HUD. Reduced motion shows the final state immediately.

Verification:

- `node --test ui/toast-motion.test.mjs ui/toast-ttl.test.mjs ui/meeting-hud.test.mjs`: 11 regressions pass, including late native acknowledgement, interrupted morph completion, restart during exit, reduced motion, continuous waveform, configured TTL and meeting stop races.
- `node scripts/qa/toast-browser.cjs`: nine real Chromium rendering checks pass. Uses synthetic voice events and native bridge; never records microphone audio. Requires Chrome, Playwright and Python Pillow for screenshot alpha inspection.
- At a sampled transition frame, the actual card width was 372.59px between the 225px recording pill and the 392px result; shell opacity stayed 1. The recovery copy action remained functional.
- Every pixel at the outer edge of all four transparent screenshots had alpha 0, including the hovered result. The listening bars changed their rendered transform and processing switched to its pulse animation.

Machine-readable evidence: [browser report](assets/toast-motion/report.json). Screenshots: [recording](assets/toast-motion/listening.png), [morph](assets/toast-motion/result-morph.png), [hovered result](assets/toast-motion/result-hover.png), [empty result](assets/toast-motion/empty.png).

These browser checks verify actual browser layout, animation and transparent pixels with synthetic IPC. macOS full-screen Space retention and real NSPanel presentation require the separate native validation; this report does not claim that browser tests prove them.
