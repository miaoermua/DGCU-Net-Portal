import { createApp } from 'vue'
import { setThemeMode } from 'miuix-vue'
import 'miuix-vue/style.css'
import './compact.css'
import App from './App.vue'

setThemeMode('system')
createApp(App).mount('#app')
