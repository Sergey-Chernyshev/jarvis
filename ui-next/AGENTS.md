# UI Next

- Keep TypeScript strict and domain types in `src/core/client/types.ts`.
- UI components call `JarvisClient`; they never import Tauri APIs directly.
- Reuse controls and tokens before adding feature-local variants.
- Preserve the functional inventory when changing a screen.
- Every visible action must produce state, progress, navigation, or feedback.
- Respect reduced motion and keep keyboard/focus behavior intact.

