# Third-party notices

Pronto includes these redistributable components:

## NVIDIA Parakeet-TDT 0.6B v3

- Copyright NVIDIA Corporation and contributors.
- Model: `nvidia/parakeet-tdt-0.6b-v3`
- License: Creative Commons Attribution 4.0 International (CC BY 4.0).
- Source: https://huggingface.co/nvidia/parakeet-tdt-0.6b-v3
- Pronto bundles NVIDIA's published Q8 GGUF conversion without modification.

## NVIDIA NeMo-Speech.cpp

- Copyright NVIDIA Corporation and contributors.
- License: Apache License 2.0.
- Source: https://github.com/NVIDIA/NeMo-Speech.cpp
- The complete runtime and transitive dependency notices are bundled under
  `runtime/nemo-speech/share/licenses/`.

Pronto communicates with DeepSeek only when the user enables cleanup and provides
an API key, or when using voice search synthesis. DeepSeek is not redistributed
with Pronto. Voice search also sends the spoken query and result snippets to the
configured DuckDuckGo HTML endpoint.

## uPlot

- Copyright (c) 2022 Leon Sorokin
- License: MIT
- Source: https://github.com/leeoniya/uPlot
- Pronto vendors `ui/vendor/uPlot.iife.min.js`, `ui/vendor/uPlot.min.css`, and
  `ui/vendor/UPLOT_LICENSE` for local chart rendering in the search window.
  No CDN is used at runtime.
