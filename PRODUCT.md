# Product

## Register

product

## Users

Solo operators and homelab users running self-hosted workloads on a single Denia node. They are technically capable (comfortable in a terminal, understand Linux runtime, namespaces, cgroups, SOPS), value direct control over their infrastructure, and dislike ceremony or hidden state. Their context: managing services, deployments, routes, secrets, and runtime metrics for their own apps, often glancing at the control plane to confirm something is healthy or to push a change. Primary surface is the admin dashboard (control-plane UI); a marketing/landing surface comes later.

## Product Purpose

Denia is a Docker-free PaaS with Denia-owned Linux runtime isolation and a single-node control plane. The dashboard exposes the `/v1` management API as a fast, legible operator UI: deploy and inspect services, manage routes and secrets, and read cgroup/procfs runtime metrics. Success is an operator trusting the tool enough to forget about it, opening it only to do a thing and leave, never to fight it.

## Brand Personality

Precise, transparent, fast. The interface shows what is actually happening under the hood, with no magic and no hidden state. Voice is direct and technical, written for someone who already understands the domain. Confidence comes from clarity and speed, not decoration. It respects the operator's time and intelligence.

## Anti-references

- **AWS/GCP console bloat**: nested tabs, walls of config, slow loads, enterprise sprawl.
- **Heroku/Vercel marketing-as-app**: gradient-heavy, oversized empty space, more pitch than tool inside the application.
- **Generic dashboard template**: card-grid of stat tiles, default sidebar + topbar boilerplate, looks like any off-the-shelf admin theme.
- **Glossy crypto/neon dark**: neon-on-black, glows, decorative glassmorphism.
- **Neo-brutalism**: thick black borders, hard drop shadows, clashing primaries as a style gimmick.

## Design Principles

- **Show the machine.** Surface real runtime state (cgroup/procfs metrics, route status, deploy progress) directly. No abstraction that hides what the node is doing.
- **Earn every pixel.** Density over decoration. If an element does not help the operator act or understand, it does not ship.
- **Fast is a feature.** The UI should feel instant. Optimistic where safe, honest where not.
- **Keyboard-respecting.** Operators live in terminals; navigation and primary actions should not require a mouse.
- **No surprises.** Destructive or irreversible actions are explicit; the UI never does more than the operator asked.

## Accessibility & Inclusion

Target WCAG AA contrast. Honor `prefers-reduced-motion` (motion is functional, never required to understand state). Full keyboard navigation with visible focus states.
