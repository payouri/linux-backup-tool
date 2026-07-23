import { useState } from 'react'

import { Button } from '@/components/ui/button'
import { useChoosePlaintext, useSetPassphrase } from '@/hooks/useEncryptionMutations'
import { useEncryptionStatus } from '@/hooks/useEncryptionStatus'

/** The literal word the plaintext override modal demands before it will honor disabling
 * Auto-insist (mirrors `crates/tui`'s `PLAINTEXT_WORD`, #61's copy sheet). */
const PLAINTEXT_WORD = 'PLAINTEXT'

/**
 * The plaintext second-confirmation modal (story 25): naming the Sensitive Categories about
 * to land in plaintext, it only calls `choose_plaintext` once the user types the literal word
 * `PLAINTEXT` — the client-only confirmation-word buffer is local component state, not a
 * global the rest of the wizard needs.
 */
function PlaintextOverrideModal({
  sensitiveCategories,
  onCancel,
  onConfirm,
}: {
  sensitiveCategories: string[]
  onCancel: () => void
  onConfirm: () => void
}) {
  const [typed, setTyped] = useState('')

  return (
    <div
      role="dialog"
      aria-label="Write credentials in plaintext?"
      className="flex flex-col gap-3 rounded-md border border-amber-500 bg-background p-4"
    >
      <p className="text-sm font-medium">These Sensitive Categories will be written unencrypted:</p>
      <ul className="text-sm">
        {sensitiveCategories.map((category) => (
          <li key={category}>• {category}</li>
        ))}
      </ul>
      <label className="flex flex-col gap-1 text-sm">
        Type {PLAINTEXT_WORD} to confirm
        <input
          value={typed}
          onChange={(event) => setTyped(event.target.value)}
          className="h-8 rounded border border-border bg-background px-2"
        />
      </label>
      <div className="flex gap-2">
        <Button variant="outline" size="sm" onClick={onCancel}>
          Cancel
        </Button>
        <Button
          variant="destructive"
          size="sm"
          disabled={typed !== PLAINTEXT_WORD}
          onClick={onConfirm}
        >
          Confirm
        </Button>
      </div>
    </div>
  )
}

/**
 * The encryption wizard step (issue #101): passphrase entry, Auto-insist (whole-archive
 * encryption forced on for a Sensitive Selection), and the override escape behind the named
 * second confirmation (story 25). Reflects `useEncryptionStatus`'s derived state; holds no
 * Selection/encryption data of its own beyond the passphrase field's transient buffer.
 */
export function EncryptionStep() {
  const { data, isPending, error } = useEncryptionStatus()
  const setPassphrase = useSetPassphrase()
  const choosePlaintext = useChoosePlaintext()
  const [passphrase, setPassphraseBuffer] = useState('')
  const [showOverrideModal, setShowOverrideModal] = useState(false)

  if (isPending) {
    return <p className="text-muted-foreground text-sm">Loading encryption status…</p>
  }

  if (error) {
    return (
      <p role="alert" className="text-destructive text-sm">
        Could not load the encryption step: {error.message}
      </p>
    )
  }

  function submitPassphrase() {
    setPassphrase.mutate(passphrase, {
      onSuccess: () => setPassphraseBuffer(''),
    })
  }

  return (
    <div className="flex flex-col gap-4">
      <h2 className="text-lg font-semibold">Encryption</h2>

      {data.isInvalidated && (
        <p role="alert" className="text-sm font-medium text-amber-600 dark:text-amber-400">
          Sensitive content added — set a passphrase.
        </p>
      )}

      {data.autoInsistRequired ? (
        <p className="text-sm">
          This selection contains Sensitive content, so the whole Archive will be encrypted.
          Sensitive Categories: {data.sensitiveCategories.join(', ')}
        </p>
      ) : (
        <p className="text-sm">
          Nothing here is Sensitive. Leave it plaintext, or set a passphrase to encrypt anyway.
        </p>
      )}

      <label className="flex flex-col gap-1 text-sm">
        Passphrase
        <input
          type="password"
          value={passphrase}
          onChange={(event) => setPassphraseBuffer(event.target.value)}
          className="h-8 rounded border border-border bg-background px-2"
        />
      </label>

      {setPassphrase.isError && (
        <p role="alert" className="text-destructive text-sm">
          {setPassphrase.error.message}
        </p>
      )}

      <div className="flex gap-2">
        <Button size="sm" onClick={submitPassphrase} disabled={setPassphrase.isPending}>
          Set passphrase
        </Button>
        {data.autoInsistRequired ? (
          <Button variant="outline" size="sm" onClick={() => setShowOverrideModal(true)}>
            Disable encryption (plaintext)
          </Button>
        ) : (
          <Button
            variant="outline"
            size="sm"
            onClick={() => choosePlaintext.mutate()}
            disabled={choosePlaintext.isPending}
          >
            Leave plaintext
          </Button>
        )}
      </div>

      {data.hasPassphrase && (
        <p className="text-muted-foreground text-sm">Passphrase set ●●●●●●●●</p>
      )}

      {showOverrideModal && (
        <PlaintextOverrideModal
          sensitiveCategories={data.sensitiveCategories}
          onCancel={() => setShowOverrideModal(false)}
          onConfirm={() => {
            choosePlaintext.mutate(undefined, {
              onSuccess: () => setShowOverrideModal(false),
            })
          }}
        />
      )}
    </div>
  )
}
