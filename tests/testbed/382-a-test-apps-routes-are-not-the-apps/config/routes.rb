Rails.application.routes.draw do
  resources :widgets, only: [:index]
end
