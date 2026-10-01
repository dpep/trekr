Rails.application.routes.draw do
  concern :batchable do
    collection { post 'batch' }
  end

  draw :admin
end
