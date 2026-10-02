module Billing
  class InvoicesController
    def create
      Billing::Invoice::Send.new
    end
  end
end
