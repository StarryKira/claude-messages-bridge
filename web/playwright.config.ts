import {defineConfig} from '@playwright/test'
export default defineConfig({
  testDir:'./tests', fullyParallel:false, workers:1, timeout:30000,
  use:{baseURL:'http://127.0.0.1:8791',headless:true,channel:process.env.PLAYWRIGHT_CHANNEL,viewport:{width:1440,height:1000}},
  webServer:{command:'python3 ../tests/console_server.py',url:'http://127.0.0.1:8791/healthz',reuseExistingServer:false,timeout:30000},
})
