class ApplicationController
  include Pundit::Authorization

  def check_authorization(model = nil)
    authorize(model)
  end
end
