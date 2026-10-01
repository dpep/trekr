class RegistrationsController < Vendor::RegistrationsController
  def after_sign_up_path_for(_resource)
    "/welcome"
  end
end
