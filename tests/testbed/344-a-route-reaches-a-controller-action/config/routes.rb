Rails.application.routes.draw do
  post 'widgets/initiate', to: 'widgets#initiate'
  get '.well-known/discovery', to: 'api/v1/oauth#discovery'

  namespace :admin do
    resources :gadgets, only: [:index, :show] do
      member do
        post :archive
      end
    end
  end
end
