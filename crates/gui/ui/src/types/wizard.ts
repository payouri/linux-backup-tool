/**
 * The four fixed wizard steps, in graph order (ADR-0007 D1: WHAT -> encryption -> WHERE ->
 * REVIEW, encryption always present). Order is conveyed by array position
 * (`WIZARD_STEPS`), mirroring `crates/tui`'s `WizardStep::ALL`.
 */
export type WizardStep = 'what' | 'encryption' | 'where' | 'review'

/** The steps in graph order — the render order of the header stepper. */
export const WIZARD_STEPS: readonly WizardStep[] = ['what', 'encryption', 'where', 'review']

export function wizardStepIndex(step: WizardStep): number {
  return WIZARD_STEPS.indexOf(step)
}

/** A step's visual state in the header stepper (mirrors `crates/tui`'s `StepStatus`). */
export type StepStatus = 'completed' | 'current' | 'locked' | 'invalidated'
