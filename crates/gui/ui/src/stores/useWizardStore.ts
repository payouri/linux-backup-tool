import { create } from 'zustand'

import type { WizardStep } from '@/types/wizard'

/**
 * Client-only wizard navigation state (issue #101): which step is current, and the WHERE
 * step's chosen (not-yet-run-against) Output location. Neither fact is sourced from `core` —
 * `core::SelectionModel` and the encryption decision stay authoritative and are reflected via
 * TanStack Query (`useSelectionSummary`/`useEncryptionStatus`); this store exists only because
 * the current step and the in-progress location pick are genuinely client-only globals shared
 * across sibling step components (WHERE and REVIEW both need the same chosen location), the
 * one case ADR-0008 names as Zustand's actual justification ("only if a genuine client-only
 * global need appears"). No Selection/encryption data is duplicated here.
 */
interface WizardStore {
  currentStep: WizardStep
  whereLocation: string | null
  setCurrentStep: (step: WizardStep) => void
  setWhereLocation: (location: string | null) => void
}

export const useWizardStore = create<WizardStore>((set) => ({
  currentStep: 'what',
  whereLocation: null,
  setCurrentStep: (step) => set({ currentStep: step }),
  setWhereLocation: (location) => set({ whereLocation: location }),
}))
