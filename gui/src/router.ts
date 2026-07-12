import { createRouter, createWebHashHistory } from 'vue-router'

const routes = [
  { path: '/', name: 'networks', component: () => import('./pages/Networks.vue') },
  { path: '/edit/:id?', name: 'edit', component: () => import('./pages/ProfileEdit.vue') },
  { path: '/status/:id', name: 'status', component: () => import('./pages/Status.vue') },
  { path: '/settings', name: 'settings', component: () => import('./pages/Settings.vue') },
  { path: '/install', name: 'install', component: () => import('./pages/Install.vue') },
]

export const router = createRouter({
  history: createWebHashHistory(),
  routes,
})
