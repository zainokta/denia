import { useEffect, useRef, useState } from 'react'

// Measures an element's content-box width via ResizeObserver so SVG charts can
// render at real pixels (crisp text + strokes) instead of a scaled viewBox.
// Returns a ref to attach and the current width (0 until first measure).
export function useResizeWidth<T extends HTMLElement>(): [
  React.RefObject<T | null>,
  number,
] {
  const ref = useRef<T | null>(null)
  const [width, setWidth] = useState(0)

  useEffect(() => {
    const el = ref.current
    if (!el) return
    setWidth(el.getBoundingClientRect().width)
    if (typeof ResizeObserver === 'undefined') return
    const ro = new ResizeObserver((entries) => {
      for (const entry of entries) {
        const w = entry.contentRect.width
        setWidth((prev) => (Math.abs(prev - w) > 0.5 ? w : prev))
      }
    })
    ro.observe(el)
    return () => ro.disconnect()
  }, [])

  return [ref, width]
}
