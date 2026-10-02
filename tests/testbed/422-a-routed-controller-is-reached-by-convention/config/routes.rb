Rails.application.routes.draw do
  resources :widgets, only: [:index]
  namespace :admin do
    resources :gadgets, only: [:index]
    namespace :tools do
      resources :parts, only: [:index]
    end
  end
end
