Rails.application.routes.draw do
  resources :reports, only: [:index] do
    collection { get 'export' }
  end
end
