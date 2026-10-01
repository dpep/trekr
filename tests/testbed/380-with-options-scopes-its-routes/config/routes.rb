Rails.application.routes.draw do
  concern :batchable do
    collection { post 'batch' }
  end

  with_options only: [:index], concerns: :batchable do
    resources :gizmos
  end
end
