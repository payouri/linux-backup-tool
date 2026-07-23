import { useEffect } from 'react'

import { useEncryptionStatus } from '@/hooks/useEncryptionStatus'
import { useSelectionSummary } from '@/hooks/useSelectionSummary'
import { useWhereCheck } from '@/hooks/useWhere'
import { useWizardStore } from '@/stores/useWizardStore'
import { WIZARD_STEPS, wizardStepIndex, type StepStatus, type WizardStep } from '@/types/wizard'

/** The verbatim gate-reason copy for a refused forward move (mirrors `crates/tui`'s
 * `GateReason`, #61's copy sheet, reused here for the same steps). */
function gateReasonFor(step: WizardStep): string | null {
  switch (step) {
    case 'what':
      return 'Nothing selected'
    case 'encryption':
      return 'Enter a passphrase'
    case 'where':
      return 'Missing backup location'
    case 'review':
      return null
  }
}

/**
 * The wizard stepper (issue #101): a completion-frontier over the derived/query state
 * `core` already exposes (ADR-0007 D2) — never a parallel client-side selection engine. Forward
 * motion is gated by the current step's completion; backward/jump to any already-reachable
 * step is free; steps ahead of the frontier are locked (ADR-0007 D2). The one cross-step
 * dependency — WHAT -> encryption via Sensitivity (ADR-0007 D5) — falls out for free because
 * `useEncryptionStatus`'s `isComplete`/`isInvalidated` are recomputed from the live Selection
 * on every refetch (triggered by `useSelectionMutations`' invalidation), not cached against a
 * point-in-time WHAT snapshot.
 *
 * Step completeness:
 * - **WHAT** <=> the Selection is non-empty (mirrors `CoreError::EmptySelection`).
 * - **encryption** <=> `useEncryptionStatus`'s `isComplete`.
 * - **WHERE** <=> a location is chosen and `useWhereCheck` resolves it without a typed error
 *   (writable, no Backup-dir collision).
 * - **REVIEW** — terminal, never "complete" (completing it is launching the run, out of this
 *   ticket's scope).
 */
export function useWizardStepper() {
  const currentStep = useWizardStore((state) => state.currentStep)
  const whereLocation = useWizardStore((state) => state.whereLocation)
  const setCurrentStep = useWizardStore((state) => state.setCurrentStep)
  const setWhereLocation = useWizardStore((state) => state.setWhereLocation)

  const summary = useSelectionSummary()
  const encryptionStatus = useEncryptionStatus()
  const whereCheck = useWhereCheck(whereLocation)

  const encryptionInvalidated = encryptionStatus.data?.isInvalidated ?? false

  const stepComplete: Record<WizardStep, boolean> = {
    what: summary.data ? !summary.data.isEmpty : false,
    encryption: encryptionStatus.data?.isComplete ?? false,
    where: whereLocation !== null && whereCheck.isSuccess,
    // Terminal: "completing" REVIEW is launching the run, not a state this stepper reaches.
    review: false,
  }

  const firstIncomplete = WIZARD_STEPS.findIndex((step) => !stepComplete[step])
  const frontierIndex = firstIncomplete === -1 ? wizardStepIndex('review') : firstIncomplete

  // While the underlying queries are still loading, every step reads as incomplete by
  // default (no data yet to prove otherwise) — that is a loading fact, not a real frontier
  // collapse, so the reestablish effect below must not act on it. Deliberately ignores
  // `whereCheck` here: it's conditionally enabled (only once a location is chosen), so its own
  // pending/idle state must never block WHAT/encryption from being considered ready.
  const dataReady = summary.isSuccess && encryptionStatus.isSuccess

  // Pulls `currentStep` back onto the frontier if a WHAT edit collapsed it below where the
  // wizard was sitting (ADR-0007 D5) — the same postcondition `crates/tui`'s
  // `reestablish_frontier` enforces after every reducer step, here run as an effect since
  // React state updates are commit-time, not synchronous within one render.
  useEffect(() => {
    if (dataReady && wizardStepIndex(currentStep) > frontierIndex) {
      setCurrentStep(WIZARD_STEPS[frontierIndex])
    }
  }, [dataReady, currentStep, frontierIndex, setCurrentStep])

  function isReachable(step: WizardStep): boolean {
    return wizardStepIndex(step) <= frontierIndex
  }

  function stepStatus(step: WizardStep): StepStatus {
    if (step === 'encryption' && encryptionInvalidated) {
      return 'invalidated'
    }
    if (step === currentStep) {
      return 'current'
    }
    return wizardStepIndex(step) < frontierIndex ? 'completed' : 'locked'
  }

  function goForward() {
    if (!stepComplete[currentStep]) {
      return
    }
    const next = WIZARD_STEPS[wizardStepIndex(currentStep) + 1]
    if (next) {
      setCurrentStep(next)
    }
  }

  function goBack() {
    const prev = WIZARD_STEPS[wizardStepIndex(currentStep) - 1]
    if (prev) {
      setCurrentStep(prev)
    }
  }

  function jumpTo(step: WizardStep) {
    if (isReachable(step)) {
      setCurrentStep(step)
    }
  }

  return {
    currentStep,
    frontierIndex,
    isReachable,
    stepStatus,
    goForward,
    goBack,
    jumpTo,
    canGoForward: stepComplete[currentStep],
    gateReason: stepComplete[currentStep] ? null : gateReasonFor(currentStep),
    whereLocation,
    setWhereLocation,
    whereCheck,
  }
}
