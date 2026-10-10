import { clsx, type ClassValue } from 'clsx'
import { twMerge } from 'tailwind-merge'
export function cn(...inputs: ClassValue[]) {
  return twMerge(clsx(inputs))
}
export function bytes(value: number) {
  if (!value) return '0 B'
  const index = Math.min(Math.floor(Math.log(value) / Math.log(1024)), 4)
  return `${(value / 1024 ** index).toLocaleString(undefined, { maximumFractionDigits: index ? 1 : 0 })} ${['B', 'KB', 'MB', 'GB', 'TB'][index]}`
}
export function date(value: number | null) {
  return value === null ? '—' : new Date(value * 1000).toLocaleString()
}
export function number(value: number) {
  return value.toLocaleString()
}
