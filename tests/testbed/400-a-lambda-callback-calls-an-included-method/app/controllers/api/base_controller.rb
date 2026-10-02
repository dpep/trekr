module Api
  class BaseController < ApplicationController
    include Api::Concerns::TokenAuth

    before_action -> { authorize! unless skip_auth? }
    after_action lambda { audited? }
  end
end
