import { Layers3 } from 'lucide-react'
import { labels, registries } from '../api'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectSeparator,
  SelectTrigger,
  SelectValue,
} from './ui/select'

export function RegistrySelect({
  value,
  onValueChange,
}: {
  value: string
  onValueChange: (value: string) => void
}) {
  return (
    <Select
      value={value || 'all'}
      onValueChange={(next) => onValueChange(next === 'all' ? '' : next)}
    >
      <SelectTrigger aria-label="Filter registry" className="w-[184px] shrink-0">
        <SelectValue />
      </SelectTrigger>
      <SelectContent align="end">
        <SelectItem value="all">
          <Layers3 className="size-3.5 text-muted-foreground" aria-hidden="true" />
          All registries
        </SelectItem>
        <SelectSeparator />
        {registries.map((kind) => (
          <SelectItem key={kind} value={kind}>
            <span
              className="registry-select-dot size-2 shrink-0 rounded-full"
              data-registry={kind}
              aria-hidden="true"
            />
            {labels[kind]}
          </SelectItem>
        ))}
      </SelectContent>
    </Select>
  )
}
