Rails.application.routes.draw do
  resources :admin_widgets, only: [:index]
  namespace :admin do
    resources :widgets, only: [:index]
  end
end
