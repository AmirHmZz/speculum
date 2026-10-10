export function BrandIcon({ size = 24 }: { size?: number }) {
  return (
    <svg
      xmlns="http://www.w3.org/2000/svg"
      width={size}
      height={size}
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth={1.8}
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
      focusable="false"
    >
      <polyline points="9,3.5 3,7 3,17 9,20.5" />
      <polyline points="3,7 9,10.5" />
      <polyline points="15,3.5 21,7 21,17 15,20.5" />
      <polyline points="21,7 15,10.5" />
      <polyline points="12,3 12,21" />
    </svg>
  )
}
