# Tilo Go-To-Market Strategy: The Engine-First Approach

## Executive Summary
Tilo is not just an operating system; it is a paradigm shift in how computing is collaborative, automated, and distributed. Rather than attempting a high-friction "OS Switch" launch, Tilo will follow a **Tiered Expansion Strategy**. We will first win the **Developer Mindshare** via a robust framework, then the **Power User Mindshare** via Desktop Environments (DE), and finally the **Market Mindshare** via a full, immutable Operating System.

**The Core Principle:** The Framework is the "Source of Truth." The DE and the OS are simply different execution environments for that framework.

---

## 1. The Core Paradigm Shift
*   **From:** Siloed, heavy applications with bolted-on collaboration.
*   **To:** Lightweight, CRDT-native applications where collaboration, state, and filesystem are system-level primitives.

---

## 2. The Tiered Rollout Strategy

### Tier 1: The Framework (The "Engine")
**Primary Focus | The Trojan Horse**
*   **Product:** The `ui` (toolkit) and `collab` (CRDT/state) crates.
*   **Target Audience:** Software Developers, Indie Hackers, Tool Builders.
*   **Value Proposition:** "Build real-time, multi-user, undoable applications without the networking or state management headache. Write once, run anywhere."
*   **GTM Channels:** GitHub, Crates.io, Hacker News, Technical Blogs, Developer YouTube/Twitter.
*   **Success Metrics:** Crate downloads, GitHub stars, number of third-party apps built using the toolkit.

### Tier 2: The Desktop Environment (The "Skin")
**Secondary Focus | The Bridge**
*   **Product:** A `shell`-based session (Wayland compositor) packaged for existing distributions (Fedora, Debian).
*   **Target Audience:** Linux enthusiasts, Tiling Window Manager (TWM) users, Power Users.
*   **Value Proposition:** "Experience the Tilo workspace—strips, collaborative filesystem, and seamless automation—on the OS you already trust."
*   **GTM Channels:** Linux Subreddits, Arch/Fedora/Debian forums, Linux gaming/enthusiast communities.
*   **Success Metrics:** Package manager installs, active sessions, community-contributed configurations.

### Tier 3: The Tilo Operating System (The "Full Stack")
**Long-Term Vision | The Destination**
*   **Product:** A complete, immutable Fedora Bootc-based OS image.
*   **Target Audience:** Security-conscious users, Enterprise/DevOps teams, "New Paradigm" seekers.
*   **Value Proposition:** "A zero-maintenance, perfectly integrated, collaborative computing environment where the hardware, OS, and apps are a single, unified entity."
*   **GTM Channels:** Enterprise sales, Tech Press, Industry Conferences, Cloud/Edge computing markets.
*   **Success Metrics:** Total system deployments, enterprise adoption, ecosystem maturity.

---

## 3. Strategic Roadmap

| Phase | Focus | Objective | Key Milestone |
| :--- | :--- | :--- | :--- |
| **Phase 1** | **Framework** | Build developer mindshare and prove the tech. | 10+ "non-Tilo" apps using the toolkit. |
| **Phase 2** | **DE** | Lower the barrier to entry for end-users. | Successful installation on a standard Debian/Fedora install. |
| **Phase 3** | **OS** | Achieve full-stack integration and scale. | The first "Bootc" image release. |

---

## 4. Risk Mitigation & Principles

1.  **The "Single Source of Truth" Rule:** Every feature implemented for the OS must be available in the Framework. We do not build "OS-only" features that developers cannot access.
2.  **The "Zero-Friction" Principle:** A developer's first experience must be "run and play" on their current OS. We avoid "The OS Switch" barrier until our ecosystem makes the switch inevitable.
3.  **The "Canary" Method:** Our internal ~40 system apps are our R&D. They are used to stress-test the framework and identify required features before they are released to the public.
