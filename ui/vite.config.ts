import { svelte } from '@sveltejs/vite-plugin-svelte'
import { defineConfig } from 'vite'

// https://vite.dev/config/
export default defineConfig({
  // Relative asset URLs so the build also works from the shell's custom protocol.
  base: './',
  plugins: [svelte()],
})
