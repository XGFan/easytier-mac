import { createApp } from 'vue'
import PrimeVue from 'primevue/config'
import ToastService from 'primevue/toastservice'
import Aura from '@primeuix/themes/aura'

import Button from 'primevue/button'
import InputText from 'primevue/inputtext'
import Textarea from 'primevue/textarea'
import Card from 'primevue/card'
import DataTable from 'primevue/datatable'
import Column from 'primevue/column'
import ToggleSwitch from 'primevue/toggleswitch'
import Tag from 'primevue/tag'
import Message from 'primevue/message'
import Dialog from 'primevue/dialog'
import ProgressSpinner from 'primevue/progressspinner'
import Toast from 'primevue/toast'

import 'primeicons/primeicons.css'
import './styles.css'

import App from './App.vue'
import { router } from './router'

const app = createApp(App)

app.use(router)
app.use(PrimeVue, {
  theme: {
    preset: Aura,
    options: {
      prefix: 'p',
      darkModeSelector: 'system',
    },
  },
})
app.use(ToastService)

app.component('Button', Button)
app.component('InputText', InputText)
app.component('Textarea', Textarea)
app.component('Card', Card)
app.component('DataTable', DataTable)
app.component('Column', Column)
app.component('ToggleSwitch', ToggleSwitch)
app.component('Tag', Tag)
app.component('Message', Message)
app.component('Dialog', Dialog)
app.component('ProgressSpinner', ProgressSpinner)
app.component('Toast', Toast)

app.mount('#app')
