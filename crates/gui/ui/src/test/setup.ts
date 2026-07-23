import '@testing-library/jest-dom/vitest'

import { cleanup } from '@testing-library/react'
import { afterEach } from 'vitest'

// React Testing Library only auto-registers its `afterEach(cleanup)` when it detects a
// *global* `afterEach` (Jest-style), which this project's `vitest.config.ts` doesn't enable
// (`test.globals` is unset; every test file imports `afterEach`/`it`/etc. from `vitest`
// explicitly instead). Without this, a `renderHook` from one test file's test case is never
// unmounted before the next runs, so any hook subscribed to *module-level* shared state (e.g.
// `useWizardStore`'s Zustand store, issue #101) keeps reacting to later tests' state changes
// long after the test that rendered it has finished — silent cross-test interference that a
// per-test-isolated `QueryClient` alone doesn't prevent.
afterEach(() => {
  cleanup()
})
