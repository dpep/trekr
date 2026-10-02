Rails.application.routes.draw do
  devise_for :users, controllers: { sessions: 'members/sessions' }
end
