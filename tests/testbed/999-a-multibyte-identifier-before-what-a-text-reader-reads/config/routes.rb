Rails.application.routes.draw do
  get 'año', to: 'widgets#index'
  get 'señal/:año', to: 'widgets#show', as: :señal
  resources :años, only: %i[índice]
end
