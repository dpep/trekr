Rails.application.routes.draw do
  resource :settings, only: [:show]
  resource :person, only: [:show]
  resource :status, only: [:show]
  resource :gadget, only: [:show]
end
