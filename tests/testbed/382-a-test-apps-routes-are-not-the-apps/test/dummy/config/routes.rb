Rails.application.routes.draw do
  get 'purge', to: 'widgets#purge'
end
