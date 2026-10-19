import { useId } from 'react'
import { Minus, Plus } from 'lucide-react'
import { Input } from './ui/input'

export function NumberField({
  label,
  hint,
  value,
  onValueChange,
  min,
  max,
  required,
  placeholder,
}: {
  label: string
  hint?: string
  value: number | null
  onValueChange: (value: number | null) => void
  min: number
  max?: number
  required?: boolean
  placeholder?: string
}) {
  const id = useId()
  const hintId = `${id}-hint`
  function step(direction: -1 | 1) {
    const next = value === null ? min : value + direction
    onValueChange(Math.min(max ?? Infinity, Math.max(min, next)))
  }
  return (
    <div className="field">
      <label className="field-label" htmlFor={id}>
        {label}
      </label>
      <div className="number-field-control">
        <Input
          id={id}
          className="number-field-input"
          type="number"
          inputMode="numeric"
          min={min}
          max={max}
          required={required}
          placeholder={placeholder}
          aria-describedby={hint ? hintId : undefined}
          value={value ?? ''}
          onChange={(event) =>
            onValueChange(event.target.value === '' ? null : Number(event.target.value))
          }
        />
        <div className="number-field-actions">
          <button
            type="button"
            aria-label={`Decrease ${label}`}
            disabled={value === null || value <= min}
            onClick={() => step(-1)}
          >
            <Minus size={13} aria-hidden="true" />
          </button>
          <button
            type="button"
            aria-label={`Increase ${label}`}
            disabled={max !== undefined && value !== null && value >= max}
            onClick={() => step(1)}
          >
            <Plus size={13} aria-hidden="true" />
          </button>
        </div>
      </div>
      {hint && (
        <span id={hintId} className="field-hint">
          {hint}
        </span>
      )}
    </div>
  )
}
