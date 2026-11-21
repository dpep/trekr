Rails.application.routes.draw do
  get "/widgets", to: "widgets#index"
  get "/w", controller: "widgets", action: "alt"
  match "/z" => "widgets#zed", via: :get
end
