Rails.application.routes.draw do
  %w[widgets gadgets].each do |name|
    resources name, only: :index
  end
end
