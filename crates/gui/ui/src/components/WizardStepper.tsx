import { Button } from '@/components/ui/button'
import { useWizardStepper } from '@/hooks/useWizardStepper'
import { cn } from '@/lib/utils'
import { WIZARD_STEPS, type WizardStep } from '@/types/wizard'

const STEP_TITLE: Record<WizardStep, string> = {
  what: 'WHAT',
  encryption: 'Encryption',
  where: 'WHERE',
  review: 'Review',
}

/**
 * The persistent horizontal stepper header (ADR-0007 D7): completed steps are jumpable, the
 * current step is highlighted, locked (ahead-of-frontier) steps are dimmed and inert, and the
 * invalidated encryption step (ADR-0007 D5) reads distinctly so a WHAT edit that reopens it is
 * visible without opening the step.
 */
export function WizardStepper({ children }: { children: React.ReactNode }) {
  const stepper = useWizardStepper()

  return (
    <div className="flex flex-col gap-4">
      <nav aria-label="Wizard steps" className="flex gap-2">
        {WIZARD_STEPS.map((step) => {
          const status = stepper.stepStatus(step)
          return (
            <button
              key={step}
              type="button"
              data-testid={`wizard-step-${step}`}
              data-status={status}
              disabled={!stepper.isReachable(step)}
              onClick={() => stepper.jumpTo(step)}
              className={cn(
                'rounded px-2 py-1 text-sm',
                status === 'current' && 'bg-primary text-primary-foreground',
                status === 'completed' && 'text-foreground underline',
                status === 'locked' && 'text-muted-foreground opacity-50',
                status === 'invalidated' && 'text-amber-600 dark:text-amber-400',
              )}
            >
              {STEP_TITLE[step]}
            </button>
          )
        })}
      </nav>

      <div>{children}</div>

      <div className="flex justify-between gap-2">
        <Button
          variant="outline"
          size="sm"
          onClick={stepper.goBack}
          disabled={stepper.currentStep === 'what'}
        >
          Back
        </Button>
        {stepper.currentStep !== 'review' && (
          <div className="flex flex-col items-end gap-1">
            {!stepper.canGoForward && stepper.gateReason && (
              <span role="alert" className="text-destructive text-xs">
                {stepper.gateReason}
              </span>
            )}
            <Button size="sm" onClick={stepper.goForward} disabled={!stepper.canGoForward}>
              Next
            </Button>
          </div>
        )}
      </div>
    </div>
  )
}
