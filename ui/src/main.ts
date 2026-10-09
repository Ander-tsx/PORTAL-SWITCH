// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The PortalSwitch Authors

import { mount } from 'svelte'
import './app.css'
import App from './App.svelte'

export default mount(App, { target: document.getElementById('app')! })
